//! Bluetooth OBEX Object Push sends (F-BT1), over BlueZ and obexd on raw
//! D-Bus. Receiving is the system's (F-BT2).
//!
//! QtBluetooth is not allowed in Harbour, so this talks to the daemons
//! directly, with the `dbus` crate over the system `libdbus-1.so.3`:
//!
//! - **BlueZ** (`org.bluez`, system bus) says which devices are paired and
//!   accept Object Push ([`bluez`]). Only those can be sent to, and the
//!   list is read again for every send.
//! - **obexd** (`org.bluez.obex`, session bus) does the push ([`obex`]).
//!
//! **F-BT2.** Only one OBEX agent can be registered with obexd, and the
//! Sailfish system UI holds it to receive files. This adapter never
//! registers an agent, never listens, and reports
//! [`crate::api::ProtocolState::SendOnly`].
//!
//! **Text.** obexd pushes files and nothing else. Sending a text would
//! mean writing it to a temporary vCard or `.txt` first, and only
//! sukkula-core writes files (S3), so a send with any text in it is
//! refused whole with `Unavailable`, before anything is sent.
//!
//! **Blocking.** `dbus` is a blocking library. Every D-Bus conversation
//! runs on a tokio blocking thread (`spawn_blocking`), never on the
//! runtime's workers, and polls its socket in 50 ms ticks so it notices
//! the transfer's token -- cancelled by the user (F-C5) or by the engine
//! stopping -- promptly. Every call has a timeout ([`obex::Timeouts`]);
//! cleanup on the way out is bounded to well inside the engine's stop
//! budget. At most [`MAX_QUERIES`] device listings run at once, and sends
//! are bounded by the engine's transfer limit.
//!
//! **Sailjail.** The `Bluetooth` permission grants talking to `org.bluez`
//! on the system bus and `org.bluez.obex` on the session bus. This module
//! calls nothing else except the bus daemon itself (`Hello`, `AddMatch`),
//! which xdg-dbus-proxy always allows. Nothing more is needed.
//!
//! **Bus addresses** come from `DBUS_SYSTEM_BUS_ADDRESS` and
//! `DBUS_SESSION_BUS_ADDRESS` (or the standard sockets), and only `unix:`
//! addresses are used: libdbus's `unixexec:` and `autolaunch:` transports
//! spawn processes (S8), so libdbus's own bus lookup is not used either.
//! Tests pass private buses through [`adapter_with`], which is not a
//! setting and not reachable from the UI.

mod bluez;
mod bus;
mod obex;

use std::sync::Arc;

use sukkula_core::Protocol;
use tokio::sync::Semaphore;

use crate::adapter::{Adapter, BoxFuture, Outgoing, OutgoingFile, unavailable};
use crate::api::{BluetoothDevice, Direction, ErrorCode, ErrorInfo, SendTarget, TransferId};
use crate::ctx::Ctx;

use self::bluez::{Address, Device};
#[doc(hidden)]
pub use self::obex::Timeouts;

/// Most device listings (each a short system-bus conversation on a
/// blocking thread) running at once.
pub const MAX_QUERIES: usize = 2;

/// Where the adapter finds D-Bus, and how long it waits.
///
/// Not a setting: the engine always uses [`BusConfig::default`], which reads
/// the standard environment. Tests point it at private buses.
#[doc(hidden)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BusConfig {
    /// The system bus address; `None` for the environment's.
    pub system_bus: Option<String>,
    /// The session bus address; `None` for the environment's.
    pub session_bus: Option<String>,
    /// The waits.
    pub timeouts: Timeouts,
}

/// The adapter.
#[must_use]
pub fn adapter(ctx: Arc<Ctx>) -> Arc<dyn Adapter> {
    adapter_with(ctx, BusConfig::default())
}

/// The adapter, on the buses `config` names. For tests.
#[doc(hidden)]
#[must_use]
pub fn adapter_with(ctx: Arc<Ctx>, config: BusConfig) -> Arc<dyn Adapter> {
    Arc::new(BluetoothAdapter {
        ctx,
        config: Arc::new(config),
        queries: Arc::new(Semaphore::new(MAX_QUERIES)),
    })
}

struct BluetoothAdapter {
    ctx: Arc<Ctx>,
    config: Arc<BusConfig>,
    queries: Arc<Semaphore>,
}

impl BluetoothAdapter {
    /// Paired Object Push devices, read from BlueZ now.
    async fn devices(&self) -> Result<Vec<Device>, ErrorInfo> {
        let busy = || ErrorInfo::new(ErrorCode::TooLarge, "too many Bluetooth requests at once");
        let permit = tokio::time::timeout(
            self.config.timeouts.call,
            self.queries.clone().acquire_owned(),
        )
        .await
        .map_err(|_| busy())?
        .map_err(|_| busy())?;
        let address = self
            .config
            .system_bus
            .clone()
            .or_else(bus::system_address_from_env)
            .ok_or_else(|| unavailable("Bluetooth"))?;
        let timeout = self.config.timeouts.call;
        let shutdown = self.ctx.shutdown_token().clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            bluez::list(&address, timeout, &shutdown)
        })
        .await
        .map_err(|_| ErrorInfo::new(ErrorCode::Internal, "the Bluetooth query stopped"))?
    }

    async fn start_send(
        &self,
        target: SendTarget,
        items: Vec<Outgoing>,
    ) -> Result<TransferId, ErrorInfo> {
        let SendTarget::Bluetooth { address } = target else {
            return Err(ErrorInfo::new(
                ErrorCode::BadCommand,
                "not a Bluetooth target",
            ));
        };
        let address = Address::parse(&address).ok_or_else(|| {
            ErrorInfo::new(ErrorCode::BadCommand, "not a Bluetooth device address")
        })?;
        let files = files_only(items)?;
        let session_bus = self
            .config
            .session_bus
            .clone()
            .or_else(bus::session_address_from_env)
            .ok_or_else(|| unavailable("the Bluetooth file service"))?;
        // Paired now, not just when the list was shown.
        let device = self
            .devices()
            .await?
            .into_iter()
            .find(|d| d.address == address)
            .ok_or_else(|| {
                ErrorInfo::new(
                    ErrorCode::NotFound,
                    "no paired device with that address accepts files",
                )
            })?;
        if !device.powered {
            return Err(ErrorInfo::new(ErrorCode::Unavailable, "Bluetooth is off"));
        }
        let views = files
            .iter()
            .map(|f| Outgoing::File(f.clone()).view())
            .collect();
        let total = files.iter().fold(0u64, |acc, f| acc.saturating_add(f.size));
        let handle = self
            .ctx
            .transfers()
            .begin_views(
                &self.ctx,
                Direction::Outgoing,
                Protocol::Bluetooth,
                &device.name,
                views,
                total,
            )
            .ok_or_else(|| ErrorInfo::new(ErrorCode::TooLarge, "too many transfers running"))?;
        let id = handle.id();
        let timeouts = self.config.timeouts;
        // Detached on purpose: the thread ends on its own, bounded by the
        // timeouts and by the transfer's token, and it finishes the handle
        // itself. Were it to panic, dropping the handle reports the transfer
        // as failed.
        drop(tokio::task::spawn_blocking(move || {
            let result = obex::send(&session_bus, device.address, &files, &timeouts, &handle);
            handle.finish_with(result.map(|()| Vec::new()));
        }));
        Ok(id)
    }
}

/// The files of a send; `Unavailable` if it holds any text (see the module
/// docs).
fn files_only(items: Vec<Outgoing>) -> Result<Vec<OutgoingFile>, ErrorInfo> {
    items
        .into_iter()
        .map(|item| match item {
            Outgoing::File(f) => Ok(f),
            Outgoing::Text(_) => Err(ErrorInfo::new(
                ErrorCode::Unavailable,
                "Bluetooth can send files only, not text",
            )),
        })
        .collect()
}

impl Adapter for BluetoothAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Bluetooth
    }

    /// F-BT2: the system UI receives.
    fn receives(&self) -> bool {
        false
    }

    fn start_receiving(&self) -> BoxFuture<'_, Result<(), ErrorInfo>> {
        // Never called while `receives` is false; if it were, it must not
        // register an OBEX agent, which would take receiving from the
        // system UI.
        Box::pin(async { Err(unavailable("receiving over Bluetooth")) })
    }

    fn stop_receiving(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn send(
        &self,
        target: SendTarget,
        items: Vec<Outgoing>,
    ) -> BoxFuture<'_, Result<TransferId, ErrorInfo>> {
        Box::pin(self.start_send(target, items))
    }

    fn list_devices(&self) -> BoxFuture<'_, Result<Vec<BluetoothDevice>, ErrorInfo>> {
        Box::pin(async {
            Ok(self
                .devices()
                .await?
                .into_iter()
                .map(|d| BluetoothDevice {
                    address: d.address.to_string(),
                    name: d.name,
                })
                .collect())
        })
    }
}
