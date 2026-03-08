#![no_main]
use libfuzzer_sys::fuzz_target;
use pulumi_kubernetes_operator::api::workspace::Workspace;

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<Workspace>(data);
});
