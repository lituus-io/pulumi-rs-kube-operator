#![no_main]
use libfuzzer_sys::fuzz_target;
use pulumi_kubernetes_operator::core::lock::is_lock_error;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = is_lock_error(s);
    }
});
