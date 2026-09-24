//! The BLE nudge (F-QS2): while Sukkula looks for Quick Share devices to
//! send to, it advertises the Quick Share "a device nearby is sharing"
//! beacon -- service data under the 16-bit UUID 0xFE2C -- so that Android
//! phones nearby with Quick Share on start announcing their mDNS service,
//! which is how discovery then finds them.
//!
//! **How.** BlueZ's `org.bluez.LEAdvertisingManager1` on the system bus,
//! with the `dbus` crate over the system libdbus-1 (the same stack as the
//! Bluetooth adapter; no second D-Bus library). BlueZ asks for the
//! advertisement's properties by calling back into an
//! `org.bluez.LEAdvertisement1` object we export, so this module answers
//! method calls on one object path while it advertises.
//!
//! **Best effort.** No adapter, BlueZ not running, the sandbox saying no,
//! BlueZ out of advertising slots: the nudge logs at debug and ends, and
//! LAN discovery goes on without it.
//!
//! **Bounded, blocking, cancellable.** `dbus` is blocking, so this runs on
//! a tokio blocking thread; it polls its socket in [`TICK`]s so it notices
//! the token within one, every wait has a deadline, and the way out
//! (unregistering) is bounded by [`UNREGISTER_WAIT`], well inside the
//! engine's stop budget. It connects only to `unix:` bus addresses: libdbus
//! would spawn a process for `unixexec:` or `autolaunch:` (S8). Names are
//! built with the dbus crate's fallible constructors from constants only,
//! never from anything received. Calls from anyone but the bus or BlueZ get
//! an error reply and change nothing, except that anyone may read the
//! advertisement's (public) properties.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use dbus::Message;
use dbus::arg::{PropMap, RefArg, Variant};
use dbus::channel::Channel;
use dbus::message::MessageType;
use dbus::strings::{BusName, ErrorName, Interface, Member, Path};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// How long one poll of the socket blocks at most.
pub const TICK: Duration = Duration::from_millis(50);

/// How long the bus may take to say hello.
const HELLO_WAIT: Duration = Duration::from_secs(2);

/// How long BlueZ may take to register the advertisement (it calls back for
/// the properties first).
const REGISTER_WAIT: Duration = Duration::from_secs(5);

/// How long unregistering may take on the way out.
pub const UNREGISTER_WAIT: Duration = Duration::from_secs(1);

/// Longest bus address accepted from the environment.
const MAX_ADDRESS_BYTES: usize = 1024;

const DBUS_NAME: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const DBUS_IFACE: &str = "org.freedesktop.DBus";
const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";
const INTROSPECTABLE_IFACE: &str = "org.freedesktop.DBus.Introspectable";

const BLUEZ_NAME: &str = "org.bluez";
/// The Jolla Phone has one Bluetooth adapter.
const ADAPTER_PATH: &str = "/org/bluez/hci0";
const MANAGER_IFACE: &str = "org.bluez.LEAdvertisingManager1";
const ADVERTISEMENT_IFACE: &str = "org.bluez.LEAdvertisement1";

/// The object BlueZ reads the advertisement from.
pub const ADVERTISEMENT_PATH: &str = "/org/sailfishos/sukkula/quickshare/nudge";

/// The 16-bit service UUID 0xFE2C, in the 128-bit form BlueZ takes.
pub const SERVICE_UUID: &str = "0000fe2c-0000-1000-8000-00805f9b34fb";

/// The Quick Share "fast initiation" service data that Android's share
/// sheet advertises while it looks for receivers, as open-quickshare sends
/// it (its `blea.rs`, removed from the vendored copy with the rest of its
/// BLE code). Nothing in it identifies this phone.
pub const SERVICE_DATA: [u8; 24] = [
    252, 18, 142, 1, 66, 0, 0, 0, 0, 0, 0, 0, 0, 0, 191, 45, 91, 160, 225, 216, 117, 36, 202, 0,
];

const INTROSPECTION: &str = concat!(
    "<!DOCTYPE node PUBLIC \"-//freedesktop//DTD D-BUS Object Introspection 1.0//EN\" ",
    "\"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd\">",
    "<node><interface name=\"org.bluez.LEAdvertisement1\">",
    "<method name=\"Release\"/>",
    "<property name=\"Type\" type=\"s\" access=\"read\"/>",
    "<property name=\"ServiceData\" type=\"a{sv}\" access=\"read\"/>",
    "</interface>",
    "<interface name=\"org.freedesktop.DBus.Properties\">",
    "<method name=\"Get\"><arg type=\"s\" direction=\"in\"/><arg type=\"s\" direction=\"in\"/>",
    "<arg type=\"v\" direction=\"out\"/></method>",
    "<method name=\"GetAll\"><arg type=\"s\" direction=\"in\"/>",
    "<arg type=\"a{sv}\" direction=\"out\"/></method>",
    "</interface></node>"
);

/// Why the nudge is not (or no longer) on the air. For debug logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NudgeError {
    /// No usable bus address, or nothing listening there.
    Unreachable,
    /// The connection dropped.
    Disconnected,
    /// No answer in time.
    Timeout,
    /// Stopped before it was on the air.
    Cancelled,
    /// An error reply, by its bounded error name.
    Remote(String),
    /// A name that could not be built, or a reply of the wrong shape.
    Malformed,
}

impl std::fmt::Display for NudgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NudgeError::Unreachable => f.write_str("system bus unreachable"),
            NudgeError::Disconnected => f.write_str("system bus disconnected"),
            NudgeError::Timeout => f.write_str("no reply in time"),
            NudgeError::Cancelled => f.write_str("cancelled"),
            NudgeError::Remote(name) => write!(f, "error reply {name}"),
            NudgeError::Malformed => f.write_str("malformed message"),
        }
    }
}

/// Starts the nudge on a blocking thread; it ends when `token` is
/// cancelled, or earlier if it cannot advertise.
pub(super) fn spawn(address: Option<String>, token: CancellationToken) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let Some(address) = address.or_else(system_address_from_env) else {
            tracing::debug!("quickshare: no system bus; no BLE nudge");
            return;
        };
        match advertise(&address, &token) {
            Ok(()) => {}
            Err(NudgeError::Cancelled) => {}
            Err(e) => tracing::debug!(error = %e, "quickshare: no BLE nudge"),
        }
    })
}

/// Registers the advertisement and serves it until `token` is cancelled or
/// BlueZ releases it, then unregisters.
///
/// # Errors
///
/// Why it could not advertise, or stopped.
pub fn advertise(address: &str, token: &CancellationToken) -> Result<(), NudgeError> {
    check_address(address)?;
    let channel = Channel::open_private(address).map_err(|_| NudgeError::Unreachable)?;
    let mut nudge = Nudge {
        channel,
        bluez: None,
        released: false,
    };
    let hello = method_call(DBUS_NAME, DBUS_PATH, DBUS_IFACE, "Hello")?;
    nudge.call(hello, HELLO_WAIT, Some(token))?;

    let register = method_call(BLUEZ_NAME, ADAPTER_PATH, MANAGER_IFACE, "RegisterAdvertisement")?
        .append2(object_path()?, PropMap::new());
    let reply = nudge.call(register, REGISTER_WAIT, Some(token))?;
    // BlueZ's unique name: the only caller whose Release counts.
    nudge.bluez = reply.sender().map(|s| s.to_string());
    tracing::debug!("quickshare: BLE nudge on the air");

    let result = nudge.serve_until(token);

    if !nudge.released
        && nudge.channel.is_connected()
        && let Ok(unregister) =
            method_call(BLUEZ_NAME, ADAPTER_PATH, MANAGER_IFACE, "UnregisterAdvertisement")
    {
        let unregister = unregister.append1(object_path()?);
        let _ = nudge.call(unregister, UNREGISTER_WAIT, None);
    }
    // Dropping the connection also makes BlueZ drop the advertisement.
    result
}

struct Nudge {
    channel: Channel,
    bluez: Option<String>,
    released: bool,
}

impl Nudge {
    /// Sends `msg` and waits for its reply, answering calls to our object
    /// meanwhile.
    fn call(
        &mut self,
        msg: Message,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<Message, NudgeError> {
        let serial = self
            .channel
            .send(msg)
            .map_err(|()| NudgeError::Disconnected)?;
        let deadline = Instant::now().checked_add(timeout);
        loop {
            while let Some(mut m) = self.channel.pop_message() {
                match m.msg_type() {
                    MessageType::MethodReturn if m.get_reply_serial() == Some(serial) => {
                        return Ok(m);
                    }
                    MessageType::Error if m.get_reply_serial() == Some(serial) => {
                        return Err(NudgeError::Remote(error_name(&mut m)));
                    }
                    MessageType::MethodCall => self.answer(&m),
                    MessageType::Signal if is_local_disconnect(&m) => {
                        return Err(NudgeError::Disconnected);
                    }
                    MessageType::Signal | MessageType::MethodReturn | MessageType::Error => {}
                }
            }
            self.wait_turn(deadline, cancel)?;
        }
    }

    /// Answers calls until `token` is cancelled, BlueZ releases the
    /// advertisement, or the bus goes away.
    fn serve_until(&mut self, token: &CancellationToken) -> Result<(), NudgeError> {
        loop {
            while let Some(m) = self.channel.pop_message() {
                match m.msg_type() {
                    MessageType::MethodCall => self.answer(&m),
                    MessageType::Signal if is_local_disconnect(&m) => {
                        return Err(NudgeError::Disconnected);
                    }
                    MessageType::Signal | MessageType::MethodReturn | MessageType::Error => {}
                }
            }
            if self.released {
                tracing::debug!("quickshare: BlueZ released the BLE nudge");
                return Ok(());
            }
            match self.wait_turn(None, Some(token)) {
                Ok(()) => {}
                Err(NudgeError::Cancelled) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    /// Answers one method call to us.
    fn answer(&mut self, call: &Message) {
        let path_ok = call.path().is_some_and(|p| &*p == ADVERTISEMENT_PATH);
        let iface = call.interface().map(|i| i.to_string()).unwrap_or_default();
        let member = call.member().map(|m| m.to_string()).unwrap_or_default();
        let reply = if !path_ok {
            error_reply(call, "org.freedesktop.DBus.Error.UnknownObject")
        } else {
            match (iface.as_str(), member.as_str()) {
                (PROPERTIES_IFACE, "GetAll") => match call.read1::<&str>() {
                    Ok(ADVERTISEMENT_IFACE) => {
                        Message::new_method_return(call).map(|r| r.append1(properties()))
                    }
                    Ok(_) => Message::new_method_return(call).map(|r| r.append1(PropMap::new())),
                    Err(_) => error_reply(call, "org.freedesktop.DBus.Error.InvalidArgs"),
                },
                (PROPERTIES_IFACE, "Get") => match call.read2::<&str, &str>() {
                    Ok((ADVERTISEMENT_IFACE, name)) => match properties().remove(name) {
                        Some(value) => Message::new_method_return(call).map(|r| r.append1(value)),
                        None => error_reply(call, "org.freedesktop.DBus.Error.InvalidArgs"),
                    },
                    _ => error_reply(call, "org.freedesktop.DBus.Error.InvalidArgs"),
                },
                (ADVERTISEMENT_IFACE, "Release") => {
                    let from_bluez = self.bluez.is_some()
                        && call.sender().map(|s| s.to_string()) == self.bluez;
                    if from_bluez {
                        self.released = true;
                        Message::new_method_return(call)
                    } else {
                        error_reply(call, "org.freedesktop.DBus.Error.AccessDenied")
                    }
                }
                (INTROSPECTABLE_IFACE, "Introspect") => {
                    Message::new_method_return(call).map(|r| r.append1(INTROSPECTION))
                }
                _ => error_reply(call, "org.freedesktop.DBus.Error.UnknownMethod"),
            }
        };
        if let Some(reply) = reply {
            let _ = self.channel.send(reply);
        }
    }

    /// One bounded turn of the socket.
    fn wait_turn(
        &self,
        deadline: Option<Instant>,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), NudgeError> {
        if !self.channel.is_connected() {
            return Err(NudgeError::Disconnected);
        }
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(NudgeError::Cancelled);
        }
        let left = match deadline {
            Some(d) => d.saturating_duration_since(Instant::now()),
            None => TICK,
        };
        if left.is_zero() {
            return Err(NudgeError::Timeout);
        }
        self.channel
            .read_write(Some(left.min(TICK)))
            .map_err(|()| NudgeError::Disconnected)
    }
}

/// The advertisement's properties: a non-connectable broadcast of the
/// service data, nothing else (no name, no address, no appearance).
#[must_use]
pub fn properties() -> PropMap {
    let mut service_data: HashMap<String, Variant<Box<dyn RefArg>>> = HashMap::new();
    service_data.insert(
        SERVICE_UUID.to_owned(),
        Variant(Box::new(SERVICE_DATA.to_vec())),
    );
    let mut props = PropMap::new();
    props.insert("Type".to_owned(), Variant(Box::new("broadcast".to_owned())));
    props.insert("ServiceData".to_owned(), Variant(Box::new(service_data)));
    props
}

fn object_path() -> Result<Path<'static>, NudgeError> {
    Path::new(ADVERTISEMENT_PATH).map_err(|_| NudgeError::Malformed)
}

/// An error reply to `call`; `None` if libdbus cannot make one.
fn error_reply(call: &Message, name: &'static str) -> Option<Message> {
    let name = ErrorName::new(name).ok()?;
    Some(call.error(&name, c"not supported"))
}

/// Builds a method call from constant names; `Malformed` if one is not
/// valid. The dbus crate's `From<&str>` conversions panic instead.
fn method_call(dest: &str, path: &str, iface: &str, member: &str) -> Result<Message, NudgeError> {
    let dest = BusName::new(dest).map_err(|_| NudgeError::Malformed)?;
    let path = Path::new(path).map_err(|_| NudgeError::Malformed)?;
    let iface = Interface::new(iface).map_err(|_| NudgeError::Malformed)?;
    let member = Member::new(member).map_err(|_| NudgeError::Malformed)?;
    Ok(Message::method_call(&dest, &path, &iface, &member))
}

/// The error name of an error reply, at most 255 bytes; never its message.
fn error_name(m: &mut Message) -> String {
    match m.as_result() {
        Err(e) => sukkula_core::text::truncate_bytes(e.name().unwrap_or(""), 255).to_owned(),
        Ok(_) => String::new(),
    }
}

/// libdbus's own note that the socket closed.
fn is_local_disconnect(m: &Message) -> bool {
    m.interface()
        .is_some_and(|i| &*i == "org.freedesktop.DBus.Local")
        && m.member().is_some_and(|n| &*n == "Disconnected")
}

/// Accepts only `unix:` transports: libdbus's `unixexec:` and
/// `autolaunch:` spawn processes (S8), and `tcp:` is a bus across the
/// network. The address comes from the environment, so it is input.
fn check_address(address: &str) -> Result<(), NudgeError> {
    if address.is_empty() || address.len() > MAX_ADDRESS_BYTES || address.contains('\0') {
        return Err(NudgeError::Unreachable);
    }
    let mut entries: usize = 0;
    for entry in address.split(';').filter(|e| !e.is_empty()) {
        if !entry.starts_with("unix:") {
            return Err(NudgeError::Unreachable);
        }
        entries = entries.saturating_add(1);
    }
    if entries == 0 {
        return Err(NudgeError::Unreachable);
    }
    Ok(())
}

/// The system bus: `$DBUS_SYSTEM_BUS_ADDRESS`, else the standard socket
/// (where Sailjail proxies it). Never libdbus's own lookup.
fn system_address_from_env() -> Option<String> {
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
    fn only_unix_addresses_are_used() {
        assert!(check_address("unix:path=/run/dbus/system_bus_socket").is_ok());
        for bad in [
            "",
            ";",
            "unixexec:path=/bin/sh",
            "autolaunch:",
            "tcp:host=192.0.2.1,port=1",
            "unix:path=/a;autolaunch:",
            "unix:path=/a\0",
        ] {
            assert_eq!(check_address(bad), Err(NudgeError::Unreachable), "{bad:?}");
        }
    }

    #[test]
    fn the_advertisement_is_the_fast_init_beacon_and_nothing_else() {
        let props = properties();
        assert_eq!(props.len(), 2);
        assert_eq!(props["Type"].0.as_str(), Some("broadcast"));
        let data = &props["ServiceData"];
        let mut iter = data.0.as_iter().unwrap();
        assert_eq!(iter.next().and_then(|k| k.as_str().map(str::to_owned)).as_deref(), Some(SERVICE_UUID));
        assert!(object_path().is_ok());
        assert!(method_call(BLUEZ_NAME, ADAPTER_PATH, MANAGER_IFACE, "RegisterAdvertisement").is_ok());
        assert_eq!(method_call("", "/", "a.b", "c").err(), Some(NudgeError::Malformed));
    }

    #[test]
    fn nothing_listening_is_unreachable_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let addr = format!("unix:path={}/none", dir.path().display());
        let t = Instant::now();
        let err = advertise(&addr, &CancellationToken::new()).unwrap_err();
        assert_eq!(err, NudgeError::Unreachable);
        assert!(t.elapsed() < Duration::from_secs(2));
    }
}
