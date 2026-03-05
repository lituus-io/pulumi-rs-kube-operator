use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ----- ProgramSpec (inner data, unchanged) -----

/// Program CRD -- pulumi.com/v1
///
/// Note: We manually define the `Program` root struct below (instead of
/// `#[derive(CustomResource)]`) because the Go operator's CRD stores the
/// program data under a top-level `program` field, not the conventional
/// `spec` field that kube-derive generates.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgramSpec {
    /// Configuration schema for the program.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<BTreeMap<String, Configuration>>,

    /// Resources to create/manage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<BTreeMap<String, Resource>>,

    /// Variables computed from expressions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::api::schema::json_value_map")]
    pub variables: Option<BTreeMap<String, serde_json::Value>>,

    /// Stack outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::api::schema::json_value_map")]
    pub outputs: Option<BTreeMap<String, serde_json::Value>>,

    /// Package dependencies (name -> version).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packages: Option<BTreeMap<String, String>>,
}

// ----- Program root struct (manually defined) -----

/// Root type for the Program CRD.
///
/// JSON layout matches the Go operator:
/// ```json
/// { "apiVersion": "pulumi.com/v1", "kind": "Program",
///   "metadata": { ... }, "program": { ... }, "status": { ... } }
/// ```
///
/// Rust code accesses `.spec` for compatibility with kube-rs conventions,
/// but serde (de)serializes it as `program`.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Program {
    #[serde(flatten)]
    pub types: Option<ProgramTypeMeta>,

    pub metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,

    /// The program data. Serialized as `program` in JSON (matching Go operator).
    #[serde(rename = "program")]
    pub spec: ProgramSpec,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ProgramStatus>,
}

/// TypeMeta fields (apiVersion + kind), flattened into Program.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ProgramTypeMeta {
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "apiVersion")]
    pub api_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

// Implement JsonSchema manually so the CRD schema has `program` (not `spec`)
impl JsonSchema for Program {
    fn schema_name() -> String {
        "Program".to_owned()
    }

    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        use schemars::schema::*;

        let spec_schema = gen.subschema_for::<ProgramSpec>();
        let status_schema = gen.subschema_for::<ProgramStatus>();

        let mut props = schemars::Map::new();
        props.insert("apiVersion".to_owned(), gen.subschema_for::<String>());
        props.insert("kind".to_owned(), gen.subschema_for::<String>());
        props.insert(
            "metadata".to_owned(),
            SchemaObject {
                instance_type: Some(InstanceType::Object.into()),
                ..Default::default()
            }
            .into(),
        );
        props.insert("program".to_owned(), spec_schema);
        props.insert("status".to_owned(), status_schema);

        SchemaObject {
            instance_type: Some(InstanceType::Object.into()),
            object: Some(Box::new(ObjectValidation {
                properties: props,
                required: ["program".to_owned()].into_iter().collect(),
                ..Default::default()
            })),
            metadata: Some(Box::new(Metadata {
                description: Some(
                    "Program is the schema for the inline YAML program API.".to_owned(),
                ),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

// Implement kube::Resource manually (same as what derive would generate)
impl kube::Resource for Program {
    type DynamicType = ();
    type Scope = kube::core::NamespaceResourceScope;

    fn group(_: &()) -> std::borrow::Cow<'_, str> {
        "pulumi.com".into()
    }

    fn kind(_: &()) -> std::borrow::Cow<'_, str> {
        "Program".into()
    }

    fn version(_: &()) -> std::borrow::Cow<'_, str> {
        "v1".into()
    }

    fn api_version(_: &()) -> std::borrow::Cow<'_, str> {
        "pulumi.com/v1".into()
    }

    fn plural(_: &()) -> std::borrow::Cow<'_, str> {
        "programs".into()
    }

    fn meta(&self) -> &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
        &self.metadata
    }

    fn meta_mut(&mut self) -> &mut k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
        &mut self.metadata
    }
}

// Implement HasSpec so code can use program.spec
impl kube::core::object::HasSpec for Program {
    type Spec = ProgramSpec;

    fn spec(&self) -> &Self::Spec {
        &self.spec
    }

    fn spec_mut(&mut self) -> &mut Self::Spec {
        &mut self.spec
    }
}

// Implement HasStatus
impl kube::core::object::HasStatus for Program {
    type Status = ProgramStatus;

    fn status(&self) -> Option<&Self::Status> {
        self.status.as_ref()
    }

    fn status_mut(&mut self) -> &mut Option<Self::Status> {
        &mut self.status
    }
}

// Implement CustomResourceExt for CRD generation
impl kube::CustomResourceExt for Program {
    fn crd_name() -> &'static str {
        "programs.pulumi.com"
    }

    fn api_resource() -> kube::core::ApiResource {
        kube::core::ApiResource::erase::<Self>(&())
    }

    fn shortnames() -> &'static [&'static str] {
        &["prog"]
    }

    fn crd() -> k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition {
        use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::*;

        let columns: Vec<CustomResourceColumnDefinition> = serde_json::from_str(
            r#"[{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}]"#,
        )
        .expect("valid printer column json");

        let gen = schemars::gen::SchemaSettings::openapi3()
            .with(|s| {
                s.inline_subschemas = true;
                s.meta_schema = None;
            })
            .with_visitor(kube::core::schema::StructuralSchemaRewriter)
            .into_generator();
        let schema = gen.into_root_schema_for::<Self>();

        let jsondata = serde_json::json!({
            "metadata": {
                "name": "programs.pulumi.com"
            },
            "spec": {
                "group": "pulumi.com",
                "scope": "Namespaced",
                "names": {
                    "kind": "Program",
                    "plural": "programs",
                    "singular": "program",
                    "shortNames": ["prog"],
                    "categories": []
                },
                "versions": [{
                    "name": "v1",
                    "served": true,
                    "storage": true,
                    "additionalPrinterColumns": columns,
                    "subresources": { "status": {} },
                    "schema": {
                        "openAPIV3Schema": schema
                    }
                }]
            }
        });

        serde_json::from_value(jsondata).expect("valid CRD")
    }
}

// Convenience constructors (matching what derive would generate)
impl Program {
    pub fn new(name: &str, spec: ProgramSpec) -> Self {
        Self {
            types: Some(ProgramTypeMeta {
                api_version: Some("pulumi.com/v1".to_owned()),
                kind: Some("Program".to_owned()),
            }),
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some(name.to_owned()),
                ..Default::default()
            },
            spec,
            status: None,
        }
    }
}

// ----- Status -----

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
pub struct ProgramStatus {
    #[serde(default, rename = "observedGeneration")]
    pub observed_generation: i64,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<Artifact>,
}

// --- Sub-types ---

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct Configuration {
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub config_type: Option<ConfigType>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::api::schema::json_value_opt")]
    pub default: Option<serde_json::Value>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub enum ConfigType {
    String,
    Number,
    #[serde(rename = "List<Number>")]
    ListNumber,
    #[serde(rename = "List<String>")]
    ListString,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct Resource {
    #[serde(rename = "type")]
    pub resource_type: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::api::schema::json_value_map")]
    pub properties: Option<BTreeMap<String, serde_json::Value>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<ResourceOptions>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub get: Option<Getter>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct ResourceOptions {
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        rename = "additionalSecretOutputs"
    )]
    pub additional_secret_outputs: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "customTimeouts"
    )]
    pub custom_timeouts: Option<CustomTimeouts>,

    #[serde(default, rename = "deleteBeforeReplace")]
    pub delete_before_replace: bool,

    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        rename = "dependsOn"
    )]
    #[schemars(schema_with = "crate::api::schema::json_value_vec")]
    pub depends_on: Vec<serde_json::Value>,

    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        rename = "ignoreChanges"
    )]
    pub ignore_changes: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::api::schema::json_value_opt")]
    pub parent: Option<serde_json::Value>,

    #[serde(default)]
    pub protect: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::api::schema::json_value_opt")]
    pub provider: Option<serde_json::Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::api::schema::json_value_map")]
    pub providers: Option<BTreeMap<String, serde_json::Value>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct CustomTimeouts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct Getter {
    pub id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::api::schema::json_value_map")]
    pub state: Option<BTreeMap<String, serde_json::Value>>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct Artifact {
    pub path: String,

    pub url: String,

    pub revision: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,

    #[serde(rename = "lastUpdateTime")]
    pub last_update_time: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
}
