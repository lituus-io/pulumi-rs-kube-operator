#![no_main]
use libfuzzer_sys::fuzz_target;

/// Constant-time comparison mirroring webhook/mod.rs.
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

fuzz_target!(|data: &[u8]| {
    // Split data into two halves and compare
    if data.len() >= 2 {
        let mid = data.len() / 2;
        let (a, b) = data.split_at(mid);
        let _ = constant_time_eq(a, b);
    }
    // Also test against empty
    let _ = constant_time_eq(data, &[]);
    let _ = constant_time_eq(&[], data);
});
