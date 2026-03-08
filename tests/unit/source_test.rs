use pulumi_kubernetes_operator::api::stack::{
    FluxSource, FluxSourceReference, ProgramReference, StackSpec,
};
use pulumi_kubernetes_operator::operator::reconcile::source::SourceKind;

fn base_spec() -> StackSpec {
    serde_json::from_str(r#"{"stack": "org/test"}"#).unwrap()
}

#[test]
fn from_spec_git_source() {
    let mut spec = base_spec();
    spec.project_repo = Some("https://github.com/test/repo".into());
    spec.branch = Some("main".into());
    let source = SourceKind::from_spec(&spec).unwrap();
    assert!(
        matches!(source, SourceKind::Git { repo, .. } if repo == "https://github.com/test/repo")
    );
}

#[test]
fn from_spec_flux_source() {
    let mut spec = base_spec();
    spec.flux_source = Some(FluxSource {
        source_ref: FluxSourceReference {
            api_version: "source.toolkit.fluxcd.io/v1".into(),
            kind: "GitRepository".into(),
            name: "my-repo".into(),
        },
        dir: None,
    });
    let source = SourceKind::from_spec(&spec).unwrap();
    assert!(matches!(source, SourceKind::Flux { .. }));
}

#[test]
fn from_spec_program_source() {
    let mut spec = base_spec();
    spec.program_ref = Some(ProgramReference {
        name: "my-program".into(),
    });
    let source = SourceKind::from_spec(&spec).unwrap();
    assert!(matches!(source, SourceKind::Program { .. }));
}

#[test]
fn from_spec_no_source_errors() {
    let spec = base_spec();
    assert!(SourceKind::from_spec(&spec).is_err());
}

#[test]
fn from_spec_multiple_sources_errors() {
    let mut spec = base_spec();
    spec.project_repo = Some("https://github.com/test/repo".into());
    spec.program_ref = Some(ProgramReference {
        name: "my-program".into(),
    });
    assert!(SourceKind::from_spec(&spec).is_err());
}
