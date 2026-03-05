#![no_main]
use libfuzzer_sys::fuzz_target;
use pulumi_kubernetes_operator::api::stack::Stack;

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<Stack>(data);
});
