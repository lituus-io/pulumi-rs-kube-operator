use std::borrow::Cow;

/// Redact known secret patterns from Pulumi stderr output.
/// Patterns: password=, token=, secret=, credential, PRIVATE KEY, JWT prefix.
pub fn redact_stderr(input: &str) -> Cow<'_, str> {
    if input.is_empty() || !needs_redaction(input) {
        return Cow::Borrowed(input);
    }
    let mut output = String::with_capacity(input.len());
    for line in input.lines() {
        if line_contains_secret(line) {
            output.push_str("[REDACTED]\n");
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    Cow::Owned(output)
}

fn needs_redaction(s: &str) -> bool {
    s.contains("password")
        || s.contains("token")
        || s.contains("secret")
        || s.contains("credential")
        || s.contains("PRIVATE KEY")
        || s.contains("eyJ") // JWT prefix (base64 of {"alg":...)
}

fn line_contains_secret(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("password=")
        || lower.contains("token=")
        || lower.contains("secret=")
        || lower.contains("credential")
        || line.contains("-----BEGIN")
        || line.contains("eyJ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_clean_stderr() {
        let input = "error: resource not found\nstack trace follows\n";
        let result = redact_stderr(input);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(&*result, input);
    }

    #[test]
    fn redact_password() {
        let result = redact_stderr("error: password=hunter2\n");
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("hunter2"));
    }

    #[test]
    fn redact_jwt() {
        let result = redact_stderr("token: eyJhbGciOiJIUzI1NiJ9.payload.sig\n");
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("eyJ"));
    }

    #[test]
    fn redact_private_key() {
        let result = redact_stderr("-----BEGIN RSA PRIVATE KEY-----\nMIIE...\n");
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("BEGIN RSA"));
    }

    #[test]
    fn mixed_lines() {
        let input = "normal output\nerror: password=abc\nmore normal output\n";
        let result = redact_stderr(input);
        assert!(result.contains("normal output"));
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("password=abc"));
        assert!(result.contains("more normal output"));
    }

    #[test]
    fn empty_string() {
        let result = redact_stderr("");
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(&*result, "");
    }

    #[test]
    fn multiline_selective() {
        let input = "line1\ntoken=abc123\nline3\nsecret=xyz\nline5\n";
        let result = redact_stderr(input);
        assert!(result.contains("line1"));
        assert!(result.contains("line3"));
        assert!(result.contains("line5"));
        assert!(!result.contains("token=abc123"));
        assert!(!result.contains("secret=xyz"));
    }
}
