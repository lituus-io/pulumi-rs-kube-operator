#![no_main]
use libfuzzer_sys::fuzz_target;
use pulumi_kubernetes_operator::agent::redact::redact_stderr;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let result = redact_stderr(s);
        // Verify: if input contained "password=", output should not
        if s.to_ascii_lowercase().contains("password=") {
            assert!(
                !result.to_ascii_lowercase().contains("password="),
                "redaction leaked password"
            );
        }
    }
});
