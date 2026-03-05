use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::{Resource, ResourceExt};

use crate::api::stack::Stack;
use crate::api::workspace::Workspace;

/// Build an OwnerReference pointing to a Stack.
pub fn stack_owner_ref(stack: &Stack, controller: bool) -> OwnerReference {
    OwnerReference {
        api_version: Stack::api_version(&()).into_owned(),
        kind: Stack::kind(&()).into_owned(),
        name: stack.name_any(),
        uid: stack.uid().unwrap_or_default(),
        controller: Some(controller),
        block_owner_deletion: Some(true),
    }
}

/// Build an OwnerReference pointing to a Workspace.
pub fn workspace_owner_ref(ws: &Workspace, controller: bool) -> OwnerReference {
    OwnerReference {
        api_version: Workspace::api_version(&()).into_owned(),
        kind: Workspace::kind(&()).into_owned(),
        name: ws.name_any(),
        uid: ws.metadata.uid.clone().unwrap_or_default(),
        controller: Some(controller),
        block_owner_deletion: Some(true),
    }
}
