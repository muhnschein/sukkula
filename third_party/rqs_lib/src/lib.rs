//! Quick Share (Nearby Share) over the LAN, as a library an application
//! embeds.
//!
//! The application owns every socket, task and timer: it accepts or opens
//! the TCP connection, wraps it in an [`InboundRequest`] or
//! [`OutboundRequest`], and drives the protocol one frame at a time, getting
//! back events (an introduction to show the user, chunks of an accepted
//! file, a text). It decides whether to accept, where bytes go and when to
//! give up. rqs_lib keeps no global state, spawns no task and never touches
//! the file system; the only thread it causes is mdns-sd's daemon, which
//! [`MDnsServer::run`] and [`MDnsDiscovery::run`] stop before they return.

#[macro_use]
extern crate log;

pub mod hdl;
pub mod utils;

pub use hdl::{
    EndpointInfo, InboundEvent, InboundRequest, MDnsDiscovery, MDnsServer, OutboundEvent,
    OutboundPayload, OutboundRequest, TransferState,
};
pub use utils::DeviceType;

// The protobuf code is generated once and committed, so that building needs
// no `protoc`. It is exactly what prost-build 0.13.5 emits for the files in
// src/proto_src/ -- the former build.rs, with `Config::out_dir` pointed here:
//
//     prost_build::Config::new()
//         .out_dir("src/proto")
//         .compile_protos(&[<the six .proto files>], &["src/proto_src"])
//
// Regenerate it whenever a .proto file or the prost version changes.
pub mod sharing_nearby {
    include!("proto/sharing.nearby.rs");
}

pub mod securemessage {
    include!("proto/securemessage.rs");
}

pub mod securegcm {
    include!("proto/securegcm.rs");
}

pub mod location_nearby_connections {
    include!("proto/location.nearby.connections.rs");
}
