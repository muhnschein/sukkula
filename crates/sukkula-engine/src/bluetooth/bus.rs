//! A small D-Bus client over one private libdbus connection, with every
//! wait bounded and every wait cancellable.
//!
//! The `dbus` crate's blocking `Connection` offers method calls that block
//! in libdbus until a reply or a timeout, with no way to stop early, and it
//! answers the bus's `Hello` with libdbus's own 25 s default. This module
//! drives the raw [`Channel`] itself instead: it sends a call, then polls
//! the socket in short ticks until the reply with that serial arrives, the
//! deadline passes, the connection drops, or the caller's token is
//! cancelled. Signals that arrive meanwhile go to the caller's handler
//! right away, so nothing is queued here and nothing is lost in order.
//!
//! Each user of this module opens its own private connection and drops it
//! when done. Dropping closes the socket, which is also how obexd learns
//! that every session this connection created is orphaned (see
//! `super::obex`).
//!
//! Bounds that are libdbus's, not ours: a single incoming message is at
//! most 128 MiB by the protocol and 32 MiB by dbus-daemon's default
//! `max_message_size`, and libdbus stops reading once 63 MiB of received
//! messages are alive. Those caps hold before this module sees a byte; the
//! parsers above it walk messages in place and copy only what they keep.

use std::fmt;
use std::time::{Duration, Instant};

use dbus::Message;
use dbus::channel::Channel;
use dbus::message::MessageType;
use dbus::strings::{BusName, Interface, Member, Path};
use tokio_util::sync::CancellationToken;

/// How long one poll of the socket blocks at most, so cancellation and
/// deadlines are noticed within this.
pub(super) const TICK: Duration = Duration::from_millis(50);

/// Longest bus address accepted, from the environment or a test.
const MAX_ADDRESS_BYTES: usize = 1024;

/// Longest error name kept from an error reply. The D-Bus specification
/// caps names at 255 bytes; this holds even if a peer ignores it.
const MAX_ERROR_NAME_BYTES: usize = 255;

/// The bus itself.
pub(super) const DBUS_NAME: &str = "org.freedesktop.DBus";
pub(super) const DBUS_PATH: &str = "/org/freedesktop/DBus";
pub(super) const DBUS_IFACE: &str = "org.freedesktop.DBus";
pub(super) const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";

/// Error names that mean "nobody is there to answer": the service is not
/// running and cannot be started, or the sandbox does not let us reach it.
const ABSENT: &[&str] = &[
    "org.freedesktop.DBus.Error.ServiceUnknown",
    "org.freedesktop.DBus.Error.NameHasNoOwner",
    "org.freedesktop.DBus.Error.AccessDenied",
    "org.freedesktop.DBus.Error.UnknownObject",
    "org.freedesktop.DBus.Error.UnknownInterface",
    "org.freedesktop.DBus.Error.UnknownMethod",
];

/// Why a bus operation did not produce a reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum BusError {
    /// The address was refused, or nothing listens there.
    Unreachable,
    /// The connection dropped.
    Disconnected,
    /// No reply before the deadline.
    Timeout,
    /// The caller's token was cancelled.
    Cancelled,
    /// An error reply, by its (bounded) error name. The error *message* is
    /// never kept: obexd puts file paths in some of them.
    Remote(String),
    /// A reply of the wrong shape, or a name we could not build.
    Malformed,
}

impl BusError {
    /// Whether this says the service is simply not there (as opposed to
    /// there and failing).
    pub(super) fn is_absent(&self) -> bool {
        match self {
            BusError::Unreachable | BusError::Disconnected => true,
            BusError::Remote(name) => {
                ABSENT.contains(&name.as_str())
                    || name.starts_with("org.freedesktop.DBus.Error.Spawn.")
            }
            BusError::Timeout | BusError::Cancelled | BusError::Malformed => false,
        }
    }

    /// Whether the object called no longer exists.
    pub(super) fn is_unknown_object(&self) -> bool {
        matches!(self, BusError::Remote(name) if name == "org.freedesktop.DBus.Error.UnknownObject")
    }
}

impl fmt::Display for BusError {
    // For debug logs: names only, never peer text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BusError::Unreachable => f.write_str("bus unreachable"),
            BusError::Disconnected => f.write_str("bus disconnected"),
            BusError::Timeout => f.write_str("no reply in time"),
            BusError::Cancelled => f.write_str("cancelled"),
            BusError::Remote(name) => write!(f, "error reply {name}"),
            BusError::Malformed => f.write_str("malformed reply"),
        }
    }
}

/// One private connection to one bus.
pub(super) struct Bus {
    channel: Channel,
}

impl fmt::Debug for Bus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bus").finish_non_exhaustive()
    }
}

impl Bus {
    /// Connects to `address` and says `Hello`, within `timeout`.
    ///
    /// libdbus connects the socket synchronously (a local Unix socket, so
    /// at once or not at all); authentication and `Hello` then run through
    /// [`call`](Self::call)'s bounded loop, not libdbus's own blocking
    /// registration.
    pub(super) fn connect(
        address: &str,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<Bus, BusError> {
        check_address(address)?;
        let channel = Channel::open_private(address).map_err(|_| BusError::Unreachable)?;
        let mut bus = Bus { channel };
        let hello = method_call(DBUS_NAME, DBUS_PATH, DBUS_IFACE, "Hello")?;
        bus.call(hello, timeout, cancel, &mut |_| {})?;
        Ok(bus)
    }

    /// Sends `msg` and waits up to `timeout` for its reply. Signals that
    /// arrive while waiting are handed to `signals`, in order.
    pub(super) fn call(
        &mut self,
        msg: Message,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
        signals: &mut dyn FnMut(&Message),
    ) -> Result<Message, BusError> {
        let serial = self
            .channel
            .send(msg)
            .map_err(|()| BusError::Disconnected)?;
        let deadline = deadline_after(timeout);
        loop {
            while let Some(mut m) = self.channel.pop_message() {
                match m.msg_type() {
                    MessageType::MethodReturn if m.get_reply_serial() == Some(serial) => {
                        return Ok(m);
                    }
                    MessageType::Error if m.get_reply_serial() == Some(serial) => {
                        return Err(BusError::Remote(error_name(&mut m)));
                    }
                    MessageType::Signal => {
                        if is_local_disconnect(&m) {
                            return Err(BusError::Disconnected);
                        }
                        signals(&m);
                    }
                    // Late replies to calls we gave up on, and calls to us:
                    // nobody here serves anything, and a caller that waits
                    // for an answer times out on its own side.
                    MessageType::MethodReturn | MessageType::Error | MessageType::MethodCall => {}
                }
            }
            self.wait_turn(deadline, cancel)?;
        }
    }

    /// As [`call`](Self::call), for callers that subscribed to nothing.
    pub(super) fn call_plain(
        &mut self,
        msg: Message,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<Message, BusError> {
        self.call(msg, timeout, cancel, &mut |_| {})
    }

    /// Handles incoming signals for up to `wait`.
    pub(super) fn pump(
        &mut self,
        wait: Duration,
        signals: &mut dyn FnMut(&Message),
    ) -> Result<(), BusError> {
        let deadline = deadline_after(wait);
        loop {
            while let Some(m) = self.channel.pop_message() {
                if m.msg_type() == MessageType::Signal {
                    if is_local_disconnect(&m) {
                        return Err(BusError::Disconnected);
                    }
                    signals(&m);
                }
            }
            match self.wait_turn(deadline, None) {
                Ok(()) => {}
                Err(BusError::Timeout) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    /// Adds a match rule on the bus and waits for it to be in place, so
    /// that nothing the rule covers can be missed by a call made after.
    pub(super) fn add_match(
        &mut self,
        rule: &str,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), BusError> {
        if rule.contains('\0') {
            return Err(BusError::Malformed);
        }
        let msg = method_call(DBUS_NAME, DBUS_PATH, DBUS_IFACE, "AddMatch")?.append1(rule);
        self.call_plain(msg, timeout, cancel).map(drop)
    }

    /// One bounded turn of the socket. `Timeout` once `deadline` passes.
    fn wait_turn(
        &self,
        deadline: Option<Instant>,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), BusError> {
        if !self.channel.is_connected() {
            return Err(BusError::Disconnected);
        }
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(BusError::Cancelled);
        }
        let left = match deadline {
            Some(d) => d.saturating_duration_since(Instant::now()),
            None => TICK,
        };
        if left.is_zero() {
            return Err(BusError::Timeout);
        }
        self.channel
            .read_write(Some(left.min(TICK)))
            .map_err(|()| BusError::Disconnected)
    }
}

/// `now + timeout`, or `None` for a timeout too far out to represent (which
/// then waits in ticks forever -- only reachable with absurd test values).
fn deadline_after(timeout: Duration) -> Option<Instant> {
    Instant::now().checked_add(timeout)
}

/// Builds a method call from names that must be valid; `Malformed` when one
/// is not. The dbus crate's own `From<&str>` conversions panic instead.
pub(super) fn method_call(
    dest: &str,
    path: &str,
    iface: &str,
    member: &str,
) -> Result<Message, BusError> {
    // An interior NUL would be cut off silently by the C string conversion
    // and the call would go somewhere else.
    if [dest, path, iface, member].iter().any(|s| s.contains('\0')) {
        return Err(BusError::Malformed);
    }
    let dest = BusName::new(dest).map_err(|_| BusError::Malformed)?;
    let path = Path::new(path).map_err(|_| BusError::Malformed)?;
    let iface = Interface::new(iface).map_err(|_| BusError::Malformed)?;
    let member = Member::new(member).map_err(|_| BusError::Malformed)?;
    Ok(Message::method_call(&dest, &path, &iface, &member))
}

/// The error name of an error reply, bounded; never its message.
fn error_name(m: &mut Message) -> String {
    match m.as_result() {
        Err(e) => {
            let name = e.name().unwrap_or("");
            sukkula_core::text::truncate_bytes(name, MAX_ERROR_NAME_BYTES).to_owned()
        }
        Ok(_) => String::new(),
    }
}

/// libdbus's own note that the socket closed. dbus-daemon refuses to relay
/// anything on the reserved `Local` path, so only libdbus can make one.
fn is_local_disconnect(m: &Message) -> bool {
    m.interface()
        .is_some_and(|i| &*i == "org.freedesktop.DBus.Local")
        && m.member().is_some_and(|n| &*n == "Disconnected")
}

/// Accepts only `unix:` transports.
///
/// libdbus also knows `unixexec:` (forks and execs a program: S8),
/// `autolaunch:` (spawns `dbus-launch`: S8), `tcp:` and `nonce-tcp:` (a bus
/// across the network) and `launchd:`. None of them has any business in a
/// phone app's bus address, and the address comes from the environment, so
/// it is checked like any other input.
pub(super) fn check_address(address: &str) -> Result<(), BusError> {
    if address.is_empty() || address.len() > MAX_ADDRESS_BYTES || address.contains('\0') {
        return Err(BusError::Unreachable);
    }
    let mut entries: usize = 0;
    for entry in address.split(';') {
        if entry.is_empty() {
            continue;
        }
        if !entry.starts_with("unix:") {
            return Err(BusError::Unreachable);
        }
        entries = entries.saturating_add(1);
    }
    if entries == 0 {
        return Err(BusError::Unreachable);
    }
    Ok(())
}

/// Escapes a value for a D-Bus address (`key=value`), as the specification
/// requires: everything outside `[-0-9A-Za-z_/.\*]` as `%XX`.
pub(super) fn escape_address_value(raw: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(raw.len());
    for &b in raw {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'/' | b'.' | b'\\' | b'*') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(
                HEX.get(usize::from(b >> 4)).copied().unwrap_or(b'0'),
            ));
            out.push(char::from(
                HEX.get(usize::from(b & 0x0F)).copied().unwrap_or(b'0'),
            ));
        }
    }
    out
}

/// The session bus: `$DBUS_SESSION_BUS_ADDRESS`, else the systemd user
/// bus under `$XDG_RUNTIME_DIR`. Never libdbus's autolaunch fallback.
pub(super) fn session_address_from_env() -> Option<String> {
    if let Some(a) = std::env::var_os("DBUS_SESSION_BUS_ADDRESS") {
        return a.into_string().ok();
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    let dir = std::path::PathBuf::from(dir);
    if !dir.is_absolute() {
        return None;
    }
    let socket = dir.join("bus");
    Some(format!(
        "unix:path={}",
        escape_address_value(socket.as_os_str().as_encoded_bytes())
    ))
}

/// The system bus: `$DBUS_SYSTEM_BUS_ADDRESS`, else the standard socket.
/// Sailjail proxies the system bus at the standard path.
pub(super) fn system_address_from_env() -> Option<String> {
    match std::env::var_os("DBUS_SYSTEM_BUS_ADDRESS") {
        Some(a) => a.into_string().ok(),
        None => Some(
            "unix:path=/run/dbus/system_bus_socket;unix:path=/var/run/dbus/system_bus_socket"
                .to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_unix_addresses_are_accepted() {
        assert!(check_address("unix:path=/run/dbus/system_bus_socket").is_ok());
        assert!(check_address("unix:abstract=/tmp/dbus-x,guid=00;").is_ok());
        assert!(check_address("unix:path=/a;unix:path=/b").is_ok());
        for bad in [
            "",
            ";",
            "unixexec:path=/bin/sh,argv0=sh",
            "autolaunch:",
            "tcp:host=192.0.2.1,port=1",
            "nonce-tcp:host=localhost,port=1",
            "launchd:env=X",
            "unix:path=/a;tcp:host=h,port=1",
            "unix:path=/a\0;unixexec:path=/bin/sh",
            "UNIX:path=/a",
        ] {
            assert_eq!(check_address(bad), Err(BusError::Unreachable), "{bad:?}");
        }
        let long = format!("unix:path=/{}", "a".repeat(MAX_ADDRESS_BYTES));
        assert_eq!(check_address(&long), Err(BusError::Unreachable));
    }

    #[test]
    fn address_values_are_escaped() {
        assert_eq!(
            escape_address_value(b"/run/user/100000/bus"),
            "/run/user/100000/bus"
        );
        assert_eq!(
            escape_address_value(b"/a b,c;d=e%"),
            "/a%20b%2cc%3bd%3de%25"
        );
        assert_eq!(escape_address_value(&[0xff, 0x00]), "%ff%00");
    }

    #[test]
    fn method_calls_refuse_bad_names_without_panicking() {
        assert!(method_call("org.bluez", "/", "org.bluez.X", "Y").is_ok());
        assert_eq!(
            method_call("org.bluez", "not a path", "a.b", "c").err(),
            Some(BusError::Malformed)
        );
        assert_eq!(
            method_call("org.bluez", "/a\0/b", "a.b", "c").err(),
            Some(BusError::Malformed)
        );
        assert_eq!(
            method_call("", "/", "a.b", "c").err(),
            Some(BusError::Malformed)
        );
        assert_eq!(
            method_call("org.bluez", "/", "nodots", "c").err(),
            Some(BusError::Malformed)
        );
    }

    #[test]
    fn absent_services_are_recognised() {
        assert!(BusError::Remote("org.freedesktop.DBus.Error.ServiceUnknown".into()).is_absent());
        assert!(
            BusError::Remote("org.freedesktop.DBus.Error.Spawn.ChildExited".into()).is_absent()
        );
        assert!(!BusError::Remote("org.bluez.obex.Error.Failed".into()).is_absent());
        assert!(!BusError::Timeout.is_absent());
        assert!(BusError::Unreachable.is_absent());
    }

    #[test]
    fn connecting_to_nothing_fails_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let addr = format!(
            "unix:path={}",
            escape_address_value(dir.path().join("none").as_os_str().as_encoded_bytes())
        );
        let started = Instant::now();
        let e = Bus::connect(&addr, Duration::from_secs(5), None).unwrap_err();
        assert!(e.is_absent(), "{e:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            Bus::connect("unixexec:path=/bin/true", Duration::from_secs(1), None).unwrap_err(),
            BusError::Unreachable
        );
    }
}
