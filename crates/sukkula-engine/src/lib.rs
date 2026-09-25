//! Sukkula's engine: one hub that takes commands from the UI and drives one
//! adapter per protocol.
//!
//! Adapters are thin: each translates its protocol library's events into
//! `sukkula-core` types and nothing more. Every protocol is a Cargo feature,
//! so each can be built and tested alone (spec §3).
//!
//! The engine owns a tokio runtime of its own, and one more thread that
//! does nothing but hand events to the sink: one at a time, in order, never
//! on a thread that called into the engine. [`Engine::stop`] returns only
//! when nothing the engine started is still running, and after it returns
//! the event sink is never called again: that is what lets the C ABI hand
//! out a raw callback. Every command is answered by exactly one
//! [`api::Event::Reply`], and at most [`api::MAX_IN_FLIGHT_COMMANDS`] are
//! held at once. `hub.rs` has the details.
//!
//! Each engine has a log of its own, to standard error, off by default and
//! switched by `Settings::logging` ([`logging`], S9).

#![forbid(unsafe_code)]

pub mod adapter;
pub mod api;
pub mod ctx;
mod hub;
pub mod logging;
mod slots;

#[cfg(feature = "bluetooth")]
pub mod bluetooth;
#[cfg(feature = "localsend")]
pub mod localsend;
#[cfg(feature = "quickshare")]
pub mod quickshare;
#[cfg(feature = "wormhole")]
pub mod wormhole;

pub use hub::{Engine, Refused};

/// The engine's version, as `Event::Started` reports it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
