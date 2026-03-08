//! Custom JSON Schema helpers for CRD fields that use serde_json::Value.
//!
//! Kubernetes CRD validation requires every field to have a `type` in the
//! OpenAPI v3 schema. `serde_json::Value` generates an empty schema `{}`,
//! which Kubernetes rejects. These helpers produce `type: object` with
//! `x-kubernetes-preserve-unknown-fields: true`.

use schemars::gen::SchemaGenerator;
use schemars::schema::{InstanceType, Schema, SchemaObject};

/// Schema for `serde_json::Value` — any value, preserved-unknown-fields.
/// No `type` constraint so it accepts strings, numbers, booleans, objects, arrays.
pub fn json_value(_gen: &mut SchemaGenerator) -> Schema {
    Schema::Object(SchemaObject {
        extensions: [(
            "x-kubernetes-preserve-unknown-fields".to_owned(),
            serde_json::Value::Bool(true),
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    })
}

/// Schema for `Option<serde_json::Value>` — nullable preserved-unknown-fields.
pub fn json_value_opt(_gen: &mut SchemaGenerator) -> Schema {
    json_value(_gen)
}

/// Schema for `Vec<serde_json::Value>` — array of any values.
pub fn json_value_vec(_gen: &mut SchemaGenerator) -> Schema {
    let items = SchemaObject {
        extensions: [(
            "x-kubernetes-preserve-unknown-fields".to_owned(),
            serde_json::Value::Bool(true),
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    Schema::Object(SchemaObject {
        instance_type: Some(InstanceType::Array.into()),
        array: Some(Box::new(schemars::schema::ArrayValidation {
            items: Some(schemars::schema::SingleOrVec::Single(Box::new(
                Schema::Object(items),
            ))),
            ..Default::default()
        })),
        ..Default::default()
    })
}

/// Schema for `BTreeMap<String, serde_json::Value>` — object with any-type values.
/// Values have no `type` constraint, accepting strings, numbers, booleans, objects, etc.
pub fn json_value_map(_gen: &mut SchemaGenerator) -> Schema {
    let value_schema = SchemaObject {
        extensions: [(
            "x-kubernetes-preserve-unknown-fields".to_owned(),
            serde_json::Value::Bool(true),
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    Schema::Object(SchemaObject {
        instance_type: Some(InstanceType::Object.into()),
        object: Some(Box::new(schemars::schema::ObjectValidation {
            additional_properties: Some(Box::new(Schema::Object(value_schema))),
            ..Default::default()
        })),
        ..Default::default()
    })
}
