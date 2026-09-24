//! The one interface every protocol module implements.
//!
//! The hub ([`crate::Engine`]) owns one [`Adapter`] per protocol in the
//! build and drives it from commands; the adapter owns its sockets, its
//! protocol library and its peer table, and reports through
//! [`crate::ctx::Ctx`]. Adapters never write files and never talk to the UI
//! except through the context.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use sukkula_core::Protocol;
use sukkula_core::name::SafeName;

use crate::api::{BluetoothDevice, ErrorCode, ErrorInfo, FileView, SendTarget, TransferId};

/// A boxed, sendable future: what the trait's methods return.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A local file to send, already checked by the hub: an absolute path to a
/// regular file within the size limits, and the name to announce for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutgoingFile {
    /// Where it is.
    pub path: PathBuf,
    /// The name peers are told, after S1 (our own names are not trusted to
    /// be printable either).
    pub name: SafeName,
    /// Its size when checked. An adapter sends at most this many bytes.
    pub size: u64,
    /// A MIME type guessed from the extension, if any.
    pub mime: Option<String>,
}

/// One checked thing to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outgoing {
    /// A file.
    File(OutgoingFile),
    /// A text, at most `MAX_MESSAGE_BYTES`.
    Text(String),
}

impl Outgoing {
    /// How the UI lists it.
    #[must_use]
    pub fn view(&self) -> FileView {
        match self {
            Outgoing::File(f) => FileView {
                name: f.name.as_str().to_owned(),
                size: f.size,
            },
            Outgoing::Text(t) => FileView {
                name: "text".to_owned(),
                size: u64::try_from(t.len()).unwrap_or(u64::MAX),
            },
        }
    }
}

/// What each protocol module provides.
pub trait Adapter: Send + Sync {
    /// Which protocol.
    fn protocol(&self) -> Protocol;

    /// Whether it receives at all. Bluetooth does not (F-BT2), and wormhole
    /// receives only by code; both report
    /// [`crate::api::ProtocolState::SendOnly`].
    fn receives(&self) -> bool {
        true
    }

    /// Starts listening. Called when the switch turns on, and again after a
    /// settings change while it is on. Must be idempotent.
    fn start_receiving(&self) -> BoxFuture<'_, Result<(), ErrorInfo>>;

    /// Stops listening and drops every connection not already accepted.
    /// Transfers already running continue until finished or cancelled.
    fn stop_receiving(&self) -> BoxFuture<'_, ()>;

    /// Starts looking for peers to send to, reporting them with
    /// `Event::PeerFound`.
    fn start_discovery(&self) -> BoxFuture<'_, Result<(), ErrorInfo>> {
        Box::pin(async { Ok(()) })
    }

    /// Stops looking.
    fn stop_discovery(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    /// Starts a send and returns its transfer id once registered; the work
    /// continues in a task of the adapter's own.
    fn send(
        &self,
        target: SendTarget,
        items: Vec<Outgoing>,
    ) -> BoxFuture<'_, Result<TransferId, ErrorInfo>>;

    /// Receives with a code (wormhole only).
    fn receive_code(&self, _code: String) -> BoxFuture<'_, Result<TransferId, ErrorInfo>> {
        Box::pin(async { Err(unavailable("receiving by code")) })
    }

    /// Lists paired devices (Bluetooth only).
    fn list_devices(&self) -> BoxFuture<'_, Result<Vec<BluetoothDevice>, ErrorInfo>> {
        Box::pin(async { Err(unavailable("listing devices")) })
    }
}

/// The error for something this adapter or build does not do.
#[must_use]
pub fn unavailable(what: &str) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Unavailable, format!("{what} is not available"))
}
