// Zero-cost enforcement lints
#![deny(clippy::clone_on_ref_ptr)]
#![deny(clippy::arc_with_non_send_sync)]

pub mod api;
pub mod core;
pub mod errors;

pub mod agent;
pub mod operator;

pub mod proto {
    pub mod agent {
        #![allow(clippy::clone_on_ref_ptr)]
        tonic::include_proto!("agent");
    }
}
