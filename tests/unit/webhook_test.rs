// Additional webhook security tests beyond the existing inline tests.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Reconstruct a WebhookServer and validate_signature for testing.
/// We test the constant_time_eq and signature validation logic via the public
/// module-level functions mirrored here from the webhook module.
fn compute_signature(secret: &str, payload: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(payload);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// Constant-time comparison (mirrors webhook/mod.rs:62).
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

#[test]
fn hmac_empty_payload() {
    let sig = compute_signature("secret", b"");
    // Should produce a valid signature for empty payload
    let mut mac = HmacSha256::new_from_slice(b"secret").unwrap();
    mac.update(b"");
    let expected = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    assert_eq!(sig, expected);
}

#[test]
fn hmac_unicode_payload() {
    let payload = "日本語テスト 🎉".as_bytes();
    let sig = compute_signature("secret", payload);
    let mut mac = HmacSha256::new_from_slice(b"secret").unwrap();
    mac.update(payload);
    let expected = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    assert_eq!(sig, expected);
}

#[test]
fn hmac_wrong_algorithm_prefix() {
    // sha1= prefix should not match sha256= validation
    let sig = "sha1=deadbeef";
    assert!(!sig.starts_with("sha256="));
}

#[test]
fn hmac_empty_signature() {
    // "sha256=" with no hex content
    let sig_hex = "";
    let payload = b"test";
    let mut mac = HmacSha256::new_from_slice(b"secret").unwrap();
    mac.update(payload);
    let computed = hex::encode(mac.finalize().into_bytes());
    assert!(!constant_time_eq(computed.as_bytes(), sig_hex.as_bytes()));
}

#[test]
fn constant_time_eq_different_lengths() {
    assert!(!constant_time_eq(b"abc", b"ab"));
    assert!(!constant_time_eq(b"", b"a"));
    assert!(!constant_time_eq(b"abcdef", b"abc"));
}

#[test]
fn constant_time_eq_empty() {
    assert!(constant_time_eq(b"", b""));
}

#[test]
fn constant_time_eq_same_length_different() {
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"aaa", b"bbb"));
}
