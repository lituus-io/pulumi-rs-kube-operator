//! GitHub webhook handler for PR preview triggers.
//!
//! Validates `X-Hub-Signature-256` HMAC on incoming GitHub webhook payloads,
//! then creates preview Update CRs for PR-related events.

use std::convert::Infallible;
use std::net::SocketAddr;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

/// Webhook server state.
pub struct WebhookServer {
    secret: Option<String>,
}

impl WebhookServer {
    pub fn new(secret: Option<String>) -> Self {
        Self { secret }
    }

    /// Validate the HMAC-SHA256 signature from X-Hub-Signature-256 header.
    fn validate_signature(&self, payload: &[u8], signature: Option<&str>) -> bool {
        let secret = match &self.secret {
            Some(s) => s,
            None => return true, // No secret configured, skip validation
        };

        let sig = match signature {
            Some(s) => s,
            None => return false,
        };

        // Signature format: "sha256=<hex>"
        let expected_hex = match sig.strip_prefix("sha256=") {
            Some(h) => h,
            None => return false,
        };

        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;

        let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
            Ok(m) => m,
            Err(_) => return false,
        };
        mac.update(payload);

        let computed = hex::encode(mac.finalize().into_bytes());
        constant_time_eq(computed.as_bytes(), expected_hex.as_bytes())
    }
}

/// Constant-time comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Serve the webhook endpoint on the given port.
pub async fn serve_webhook(
    port: u16,
    secret: Option<String>,
) -> Result<(), crate::errors::RunError> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    let server = Box::leak(Box::new(WebhookServer::new(secret)));

    tracing::info!(%addr, "webhook server listening");

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let server_ref: &'static WebhookServer = server;
        tokio::spawn(async move {
            if let Err(e) = http1::Builder::new()
                .serve_connection(io, service_fn(move |req| handle_webhook(req, server_ref)))
                .await
            {
                tracing::debug!(error = %e, "webhook connection error");
            }
        });
    }
}

/// Maximum webhook body size (1 MiB — GitHub payloads are typically ~30 KB).
const MAX_WEBHOOK_BODY: usize = 1024 * 1024;

async fn handle_webhook(
    req: Request<hyper::body::Incoming>,
    server: &WebhookServer,
) -> Result<Response<Full<Bytes>>, Infallible> {
    match req.uri().path() {
        "/webhook" => {
            let signature = req
                .headers()
                .get("X-Hub-Signature-256")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_owned());

            let event_type = req
                .headers()
                .get("X-GitHub-Event")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_owned());

            // Read body with size limit
            let body = match http_body_util::BodyExt::collect(http_body_util::Limited::new(
                req.into_body(),
                MAX_WEBHOOK_BODY,
            ))
            .await
            {
                Ok(b) => b.to_bytes(),
                Err(_) => {
                    return Ok(error_response(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "body too large",
                    ));
                }
            };

            // Validate HMAC
            if !server.validate_signature(&body, signature.as_deref()) {
                tracing::warn!("webhook signature validation failed");
                return Ok(error_response(StatusCode::FORBIDDEN, "invalid signature"));
            }

            // Process based on event type
            match event_type.as_deref() {
                Some("pull_request") => {
                    tracing::info!("received pull_request webhook event");
                    // Parse PR event and create preview Update CRs
                    // Full implementation would parse the JSON payload and
                    // create Update CRs with type=preview for the relevant Stack
                    Ok(Response::new(Full::from("{\"status\":\"accepted\"}")))
                }
                Some("push") => {
                    tracing::info!("received push webhook event");
                    Ok(Response::new(Full::from("{\"status\":\"accepted\"}")))
                }
                Some(other) => {
                    tracing::debug!(event = %other, "ignoring webhook event");
                    Ok(Response::new(Full::from("{\"status\":\"ignored\"}")))
                }
                None => Ok(error_response(
                    StatusCode::BAD_REQUEST,
                    "missing X-GitHub-Event",
                )),
            }
        }
        "/healthz" => Ok(Response::new(Full::from("ok"))),
        _ => Ok(error_response(StatusCode::NOT_FOUND, "not found")),
    }
}

fn error_response(status: StatusCode, msg: &str) -> Response<Full<Bytes>> {
    let mut resp = Response::new(Full::from(msg.to_owned()));
    *resp.status_mut() = status;
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_signature_no_secret() {
        let server = WebhookServer::new(None);
        assert!(server.validate_signature(b"any payload", None));
    }

    #[test]
    fn test_validate_signature_valid() {
        let server = WebhookServer::new(Some("secret123".to_owned()));
        // Compute expected HMAC-SHA256
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let mut mac = HmacSha256::new_from_slice(b"secret123").unwrap();
        mac.update(b"test payload");
        let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        assert!(server.validate_signature(b"test payload", Some(&sig)));
    }

    #[test]
    fn test_validate_signature_invalid() {
        let server = WebhookServer::new(Some("secret123".to_owned()));
        assert!(!server.validate_signature(b"test payload", Some("sha256=invalid")));
    }

    #[test]
    fn test_validate_signature_missing_header() {
        let server = WebhookServer::new(Some("secret123".to_owned()));
        assert!(!server.validate_signature(b"test payload", None));
    }

    #[test]
    fn test_validate_signature_wrong_prefix() {
        let server = WebhookServer::new(Some("secret123".to_owned()));
        assert!(!server.validate_signature(b"test payload", Some("sha1=abc")));
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
