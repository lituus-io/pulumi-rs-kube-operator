use std::borrow::Cow;

use pulumi_kubernetes_operator::agent::redact::redact_stderr;

#[test]
fn redact_stderr_passthrough() {
    let input = "error: resource not found\nstack trace follows\n";
    let result = redact_stderr(input);
    assert!(
        matches!(result, Cow::Borrowed(_)),
        "expected zero-alloc fast path"
    );
    assert_eq!(&*result, input);
}

#[test]
fn redact_stderr_password() {
    let result = redact_stderr("error: password=hunter2");
    assert!(result.contains("[REDACTED]"));
    assert!(!result.contains("hunter2"));
}

#[test]
fn redact_stderr_jwt() {
    let result = redact_stderr("token: eyJhbGciOiJIUzI1NiJ9.payload.sig");
    assert!(result.contains("[REDACTED]"));
    assert!(!result.contains("eyJhbGci"));
}

#[test]
fn redact_stderr_private_key() {
    let result = redact_stderr(
        "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKC...\n-----END RSA PRIVATE KEY-----",
    );
    assert!(result.contains("[REDACTED]"));
    assert!(!result.contains("BEGIN RSA"));
}

#[test]
fn redact_stderr_mixed() {
    let input = "normal output\nerror: password=abc\nmore normal\n";
    let result = redact_stderr(input);
    assert!(result.contains("normal output"));
    assert!(result.contains("more normal"));
    assert!(result.contains("[REDACTED]"));
    assert!(!result.contains("password=abc"));
}

#[test]
fn redact_stderr_empty() {
    let result = redact_stderr("");
    assert!(matches!(result, Cow::Borrowed(_)));
    assert_eq!(&*result, "");
}

#[test]
fn redact_stderr_multiline() {
    let input = "line1\ntoken=secret123\nline3\nsecret=xyz789\nline5\n";
    let result = redact_stderr(input);
    assert!(result.contains("line1"));
    assert!(result.contains("line3"));
    assert!(result.contains("line5"));
    assert!(!result.contains("token=secret123"));
    assert!(!result.contains("secret=xyz789"));
}

#[test]
fn redact_stderr_credential_keyword() {
    let result = redact_stderr("setting credential for registry");
    assert!(result.contains("[REDACTED]"));
}
