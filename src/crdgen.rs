//! CRD YAML generator.
//!
//! Usage: cargo run --bin crdgen
//! Prints all CRD manifests to stdout, separated by `---`.

use kube::CustomResourceExt;

fn main() {
    let crds = [
        serde_yaml::to_string(&pulumi_kubernetes_operator::api::stack::Stack::crd())
            .expect("Stack CRD"),
        serde_yaml::to_string(&pulumi_kubernetes_operator::api::workspace::Workspace::crd())
            .expect("Workspace CRD"),
        serde_yaml::to_string(&pulumi_kubernetes_operator::api::update::Update::crd())
            .expect("Update CRD"),
        serde_yaml::to_string(&pulumi_kubernetes_operator::api::program::Program::crd())
            .expect("Program CRD"),
    ];

    for (i, crd) in crds.iter().enumerate() {
        if i > 0 {
            println!("---");
        }
        print!("{crd}");
    }
}
