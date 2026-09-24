//! Paired devices that accept Object Push, from BlueZ on the system bus.
//!
//! One `GetManagedObjects` call on `org.bluez` at `/` returns every object
//! BlueZ has: adapters, paired devices, and every device merely *seen*
//! during a scan. The last kind is attacker-made: anyone in radio range can
//! advertise thousands of fake devices with any name. The reply is
//! therefore walked in place with fixed caps on how many objects,
//! interfaces, properties and UUIDs are looked at, and only the few fields
//! that decide eligibility are copied out. Names come from the remote
//! device over the air and go through S2 (`sukkula_core::text::display`).
//!
//! A device is listed only when BlueZ says it is `Paired`, not `Blocked`,
//! has a BR/EDR (public) address, and advertises the OBEX Object Push
//! service class. A send is only ever made to an address on this list,
//! re-read at send time (`super::BluetoothAdapter::send`).

use std::fmt;
use std::time::Duration;

use dbus::Message;
use dbus::arg::{ArgType, Iter};
use dbus::strings::Path;
use sukkula_core::limits::MAX_ALIAS_CHARS;
use sukkula_core::text;
use tokio_util::sync::CancellationToken;

use super::bus::{self, Bus, BusError};
use crate::api::{ErrorCode, ErrorInfo};

/// The OBEX Object Push service class (Bluetooth assigned number 0x1105).
pub(super) const OPP_UUID: &str = "00001105-0000-1000-8000-00805f9b34fb";

const BLUEZ_NAME: &str = "org.bluez";
const DEVICE_IFACE: &str = "org.bluez.Device1";
const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const OBJECT_MANAGER_IFACE: &str = "org.freedesktop.DBus.ObjectManager";

/// Most devices listed. More paired Object Push devices than this on one
/// phone is not a real configuration.
pub(super) const MAX_DEVICES: usize = 64;

/// Most objects in the reply looked at. A phone has one adapter and a few
/// dozen paired devices; the rest would be scan results, which an attacker
/// can multiply to push the paired devices past this cap (the reply's order
/// is BlueZ's hash order). The cap is high because it costs nothing: the
/// walk is linear in the reply's size, which dbus-daemon caps at 32 MiB,
/// and a 24 MB reply of 60 000 devices walks in about 0.2 s (x86 host,
/// release build).
pub(super) const MAX_OBJECTS: usize = 65_536;

/// Most interfaces looked at per object (BlueZ uses at most a handful).
const MAX_INTERFACES: usize = 32;

/// Most properties looked at per interface (Device1 has about 25).
const MAX_PROPERTIES: usize = 64;

/// Most UUIDs looked at per device.
const MAX_UUIDS: usize = 128;

/// Most adapters remembered.
const MAX_ADAPTERS: usize = 16;

/// Most eligible devices collected before sorting and cutting to
/// [`MAX_DEVICES`].
const MAX_CANDIDATES: usize = 256;

/// Longest raw name looked at before S2. BlueZ's own cap is 248 bytes (the
/// HCI name length); this holds whatever BlueZ does.
const MAX_RAW_NAME_BYTES: usize = 1024;

/// A Bluetooth device address, `AA:BB:CC:DD:EE:FF`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Address([u8; 6]);

impl Address {
    /// Parses exactly `HH:HH:HH:HH:HH:HH`, hex in either case. Refuses the
    /// three reserved addresses (any, local, all), which are not devices.
    pub(super) fn parse(s: &str) -> Option<Address> {
        let bytes = s.as_bytes();
        if bytes.len() != 17 {
            return None;
        }
        let mut out = [0u8; 6];
        for (i, slot) in out.iter_mut().enumerate() {
            let at = i.checked_mul(3)?;
            let hi = hex_digit(*bytes.get(at)?)?;
            let lo = hex_digit(*bytes.get(at.checked_add(1)?)?)?;
            *slot = (hi << 4) | lo;
            if i < 5 && *bytes.get(at.checked_add(2)?)? != b':' {
                return None;
            }
        }
        const RESERVED: [[u8; 6]; 3] = [
            [0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0xff, 0xff, 0xff],
            [0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        ];
        if RESERVED.contains(&out) {
            return None;
        }
        Some(Address(out))
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02X}:{b:02X}:{c:02X}:{d:02X}:{e:02X}:{g:02X}")
    }
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => b.checked_sub(b'0'),
        b'a'..=b'f' => b.checked_sub(b'a')?.checked_add(10),
        b'A'..=b'F' => b.checked_sub(b'A')?.checked_add(10),
        _ => None,
    }
}

/// A paired device that accepts Object Push.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Device {
    /// Its address.
    pub address: Address,
    /// Its name, after S2; its address when it has no showable name.
    pub name: String,
    /// Whether the adapter it is paired through is powered.
    pub powered: bool,
}

/// Reads the devices from BlueZ.
pub(super) fn list(
    address: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Vec<Device>, ErrorInfo> {
    let mut bus = Bus::connect(address, timeout, Some(cancel)).map_err(|e| failure(&e))?;
    let call = bus::method_call(BLUEZ_NAME, "/", OBJECT_MANAGER_IFACE, "GetManagedObjects")
        .map_err(|e| failure(&e))?;
    let reply = bus
        .call_plain(call, timeout, Some(cancel))
        .map_err(|e| failure(&e))?;
    parse_managed_objects(&reply).ok_or_else(|| {
        ErrorInfo::new(
            ErrorCode::Unavailable,
            "the Bluetooth service gave an unexpected reply",
        )
    })
}

fn failure(e: &BusError) -> ErrorInfo {
    tracing::debug!(error = %e, "BlueZ query failed");
    let message = match e {
        BusError::Cancelled => "stopping",
        BusError::Timeout => "the Bluetooth service did not answer",
        _ => "the Bluetooth service is not available",
    };
    ErrorInfo::new(ErrorCode::Unavailable, message)
}

/// What the walk collects about one `Device1`.
#[derive(Default)]
struct RawDevice {
    address: Option<Address>,
    address_seen: bool,
    alias: Option<String>,
    name: Option<String>,
    paired: bool,
    blocked: bool,
    random_address: bool,
    opp: bool,
    adapter: Option<String>,
}

#[derive(Default)]
struct Scan {
    candidates: Vec<RawDevice>,
    powered_adapters: Vec<String>,
}

/// Walks a `GetManagedObjects` reply (`a{oa{sa{sv}}}`) and returns the
/// eligible devices, deduplicated by address, sorted by name, at most
/// [`MAX_DEVICES`]. `None` when the reply is not a dictionary at all.
///
/// Any element of the wrong shape is skipped, and a property of the wrong
/// type counts as absent -- which for `Paired` and the UUIDs means "not
/// eligible", so every surprise fails closed.
pub(super) fn parse_managed_objects(msg: &Message) -> Option<Vec<Device>> {
    let mut top = msg.iter_init();
    let mut objects = top.recurse(ArgType::Array)?;
    let mut scan = Scan::default();
    let mut seen: usize = 0;
    while objects.arg_type() == ArgType::DictEntry && seen < MAX_OBJECTS {
        seen = seen.saturating_add(1);
        if let Some(mut entry) = objects.recurse(ArgType::DictEntry) {
            let path = entry.get::<Path<'_>>();
            entry.next();
            if let (Some(path), Some(interfaces)) = (path, entry.recurse(ArgType::Array)) {
                walk_object(&path, interfaces, &mut scan);
            }
        }
        objects.next();
    }
    Some(finish(scan))
}

fn walk_object(path: &str, mut interfaces: Iter<'_>, scan: &mut Scan) {
    let mut n: usize = 0;
    while interfaces.arg_type() == ArgType::DictEntry && n < MAX_INTERFACES {
        n = n.saturating_add(1);
        if let Some(mut entry) = interfaces.recurse(ArgType::DictEntry) {
            let name = entry.get::<&str>();
            entry.next();
            match (name, entry.recurse(ArgType::Array)) {
                (Some(DEVICE_IFACE), Some(props)) => {
                    let dev = read_device(props);
                    if eligible(&dev) && scan.candidates.len() < MAX_CANDIDATES {
                        scan.candidates.push(dev);
                    }
                }
                (Some(ADAPTER_IFACE), Some(props))
                    if scan.powered_adapters.len() < MAX_ADAPTERS && adapter_powered(props) =>
                {
                    scan.powered_adapters.push(path.to_owned());
                }
                _ => {}
            }
        }
        interfaces.next();
    }
}

/// Calls `f` with each property's name and the inside of its variant, for
/// at most [`MAX_PROPERTIES`] entries of an `a{sv}`.
pub(super) fn for_each_property<'a>(mut dict: Iter<'a>, mut f: impl FnMut(&'a str, Iter<'a>)) {
    let mut n: usize = 0;
    while dict.arg_type() == ArgType::DictEntry && n < MAX_PROPERTIES {
        n = n.saturating_add(1);
        if let Some(mut entry) = dict.recurse(ArgType::DictEntry)
            && let Some(key) = entry.get::<&str>()
        {
            entry.next();
            if let Some(value) = entry.recurse(ArgType::Variant) {
                f(key, value);
            }
        }
        dict.next();
    }
}

fn read_device(props: Iter<'_>) -> RawDevice {
    let mut dev = RawDevice::default();
    // The first occurrence of a key counts; a repeated key (legal on the
    // wire, never sent by BlueZ) cannot override what was already read.
    let mut seen_paired = false;
    let mut seen_blocked = false;
    let mut seen_type = false;
    let mut seen_uuids = false;
    for_each_property(props, |key, mut value| match key {
        "Address" if !dev.address_seen => {
            dev.address_seen = true;
            dev.address = value.get::<&str>().and_then(Address::parse);
        }
        "Alias" if dev.alias.is_none() => {
            dev.alias = value.get::<&str>().map(showable);
        }
        "Name" if dev.name.is_none() => {
            dev.name = value.get::<&str>().map(showable);
        }
        "Paired" if !seen_paired => {
            seen_paired = true;
            dev.paired = value.get::<bool>() == Some(true);
        }
        "Blocked" if !seen_blocked => {
            seen_blocked = true;
            // Unreadable counts as blocked.
            dev.blocked = value.get::<bool>() != Some(false);
        }
        "AddressType" if !seen_type => {
            seen_type = true;
            dev.random_address = value.get::<&str>() != Some("public");
        }
        "UUIDs" if !seen_uuids => {
            seen_uuids = true;
            dev.opp = has_opp(&mut value);
        }
        "Adapter" if dev.adapter.is_none() => {
            dev.adapter = value.get::<Path<'_>>().map(|p| p.to_string());
        }
        _ => {}
    });
    dev
}

fn has_opp(value: &mut Iter<'_>) -> bool {
    let Some(mut uuids) = value.recurse(ArgType::Array) else {
        return false;
    };
    let mut n: usize = 0;
    while uuids.arg_type() == ArgType::String && n < MAX_UUIDS {
        n = n.saturating_add(1);
        if uuids
            .get::<&str>()
            .is_some_and(|u| u.eq_ignore_ascii_case(OPP_UUID))
        {
            return true;
        }
        uuids.next();
    }
    false
}

fn adapter_powered(props: Iter<'_>) -> bool {
    let mut powered = None;
    for_each_property(props, |key, mut value| {
        if key == "Powered" && powered.is_none() {
            powered = Some(value.get::<bool>() == Some(true));
        }
    });
    powered == Some(true)
}

fn eligible(dev: &RawDevice) -> bool {
    dev.address.is_some() && dev.paired && !dev.blocked && !dev.random_address && dev.opp
}

/// S2, after a cap on how much of the raw string is even looked at.
fn showable(raw: &str) -> String {
    text::display(
        text::truncate_bytes(raw, MAX_RAW_NAME_BYTES),
        MAX_ALIAS_CHARS,
    )
}

fn finish(scan: Scan) -> Vec<Device> {
    let mut out: Vec<Device> = Vec::with_capacity(scan.candidates.len().min(MAX_DEVICES));
    for raw in scan.candidates {
        let Some(address) = raw.address else {
            continue;
        };
        let powered = raw
            .adapter
            .as_ref()
            .is_some_and(|a| scan.powered_adapters.contains(a));
        // BlueZ defaults Alias to Name, or to the address with dashes.
        let name = [raw.alias, raw.name]
            .into_iter()
            .flatten()
            .find(|n| !n.is_empty())
            .unwrap_or_else(|| address.to_string());
        match out.iter_mut().find(|d| d.address == address) {
            // Paired through two adapters: one entry, sendable if either is
            // powered.
            Some(existing) => existing.powered |= powered,
            None => out.push(Device {
                address,
                name,
                powered,
            }),
        }
    }
    out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.address.cmp(&b.address))
    });
    out.truncate(MAX_DEVICES);
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use dbus::arg::{PropMap, RefArg, Variant};
    use proptest::prelude::*;

    use super::*;

    fn v<T: RefArg + 'static>(t: T) -> Variant<Box<dyn RefArg>> {
        Variant(Box::new(t))
    }

    fn device(addr: &str, alias: &str, paired: bool, uuids: &[&str]) -> PropMap {
        let mut p = PropMap::new();
        p.insert("Address".into(), v(addr.to_owned()));
        p.insert("AddressType".into(), v("public".to_owned()));
        p.insert("Alias".into(), v(alias.to_owned()));
        p.insert("Paired".into(), v(paired));
        p.insert("Blocked".into(), v(false));
        p.insert(
            "UUIDs".into(),
            v(uuids
                .iter()
                .map(|u| (*u).to_owned())
                .collect::<Vec<String>>()),
        );
        p.insert(
            "Adapter".into(),
            v(Path::new("/org/bluez/hci0").unwrap().into_static()),
        );
        p
    }

    fn adapter(powered: bool) -> PropMap {
        let mut p = PropMap::new();
        p.insert("Powered".into(), v(powered));
        p
    }

    type Objects = HashMap<Path<'static>, HashMap<String, PropMap>>;

    fn reply(objects: Objects) -> Message {
        Message::new_signal("/", "org.example.Test", "Reply")
            .unwrap()
            .append1(objects)
    }

    fn obj(objects: &mut Objects, path: &str, iface: &str, props: PropMap) {
        objects
            .entry(Path::new(path).unwrap())
            .or_default()
            .insert(iface.to_owned(), props);
    }

    #[test]
    fn addresses_parse_strictly() {
        let a = Address::parse("aa:bb:cc:dd:ee:0f").unwrap();
        assert_eq!(a.to_string(), "AA:BB:CC:DD:EE:0F");
        assert_eq!(Address::parse("AA:BB:CC:DD:EE:0F"), Some(a));
        for bad in [
            "",
            "AA:BB:CC:DD:EE",
            "AA:BB:CC:DD:EE:FF:",
            "AA-BB-CC-DD-EE-FF",
            "AA:BB:CC:DD:EE:GG",
            " AA:BB:CC:DD:EE:F",
            "AA:BB:CC:DD:EE:F\0",
            "AABBCCDDEEFF00000",
            "AA:BB:CC:DD:EE:FＦ",
            "00:00:00:00:00:00",
            "00:00:00:FF:FF:FF",
            "ff:ff:ff:ff:ff:ff",
            "+A:BB:CC:DD:EE:FF",
        ] {
            assert_eq!(Address::parse(bad), None, "{bad:?}");
        }
    }

    proptest! {
        #[test]
        fn address_parse_never_panics_and_round_trips(s in "\\PC{0,24}") {
            if let Some(a) = Address::parse(&s) {
                prop_assert_eq!(Address::parse(&a.to_string()), Some(a));
                prop_assert!(s.eq_ignore_ascii_case(&a.to_string()));
            }
        }

        #[test]
        fn any_six_bytes_round_trip(b in any::<[u8; 6]>()) {
            let s = Address(b).to_string();
            prop_assert_eq!(s.len(), 17);
            if let Some(a) = Address::parse(&s) {
                prop_assert_eq!(a, Address(b));
            }
        }

        #[test]
        fn hostile_names_come_out_showable(name in "\\PC{0,300}|[\\u{200B}-\\u{206F}\\u{202A}-\\u{202E}a]{0,300}") {
            let mut o = Objects::new();
            obj(&mut o, "/org/bluez/hci0", ADAPTER_IFACE, adapter(true));
            obj(&mut o, "/org/bluez/hci0/dev_1", DEVICE_IFACE, device("11:22:33:44:55:66", &name, true, &[OPP_UUID]));
            let devices = parse_managed_objects(&reply(o)).unwrap();
            prop_assert_eq!(devices.len(), 1);
            let n = &devices[0].name;
            prop_assert!(!n.is_empty());
            prop_assert!(n.chars().count() <= MAX_ALIAS_CHARS);
            prop_assert!(!n.chars().any(text::is_forbidden), "{:?}", n);
        }
    }

    #[test]
    fn only_paired_unblocked_opp_devices_are_listed() {
        let mut o = Objects::new();
        obj(&mut o, "/org/bluez/hci0", ADAPTER_IFACE, adapter(true));
        obj(
            &mut o,
            "/org/bluez/hci0/dev_1",
            DEVICE_IFACE,
            device("11:22:33:44:55:66", "Paired phone", true, &[OPP_UUID]),
        );
        obj(
            &mut o,
            "/org/bluez/hci0/dev_2",
            DEVICE_IFACE,
            device("11:22:33:44:55:67", "Stranger", false, &[OPP_UUID]),
        );
        obj(
            &mut o,
            "/org/bluez/hci0/dev_3",
            DEVICE_IFACE,
            device(
                "11:22:33:44:55:68",
                "Headset",
                true,
                &["0000110b-0000-1000-8000-00805f9b34fb"],
            ),
        );
        let mut blocked = device("11:22:33:44:55:69", "Blocked", true, &[OPP_UUID]);
        blocked.insert("Blocked".into(), v(true));
        obj(&mut o, "/org/bluez/hci0/dev_4", DEVICE_IFACE, blocked);
        let mut random = device("51:22:33:44:55:6A", "LE thing", true, &[OPP_UUID]);
        random.insert("AddressType".into(), v("random".to_owned()));
        obj(&mut o, "/org/bluez/hci0/dev_5", DEVICE_IFACE, random);
        let mut typo = device("11:22:33:44:55:6B", "Wrong types", true, &[OPP_UUID]);
        typo.insert("Paired".into(), v("true".to_owned()));
        obj(&mut o, "/org/bluez/hci0/dev_6", DEVICE_IFACE, typo);
        let mut uuid_str = device("11:22:33:44:55:6C", "UUID string", true, &[]);
        uuid_str.insert("UUIDs".into(), v(OPP_UUID.to_owned()));
        obj(&mut o, "/org/bluez/hci0/dev_7", DEVICE_IFACE, uuid_str);
        let mut no_blocked = device("11:22:33:44:55:6D", "No Blocked", true, &[OPP_UUID]);
        no_blocked.insert("Blocked".into(), v(0u32));
        obj(&mut o, "/org/bluez/hci0/dev_8", DEVICE_IFACE, no_blocked);
        obj(
            &mut o,
            "/org/bluez/hci0/dev_9",
            DEVICE_IFACE,
            device("11:22:33:44:55", "Short address", true, &[OPP_UUID]),
        );
        let devices = parse_managed_objects(&reply(o)).unwrap();
        assert_eq!(
            devices,
            vec![Device {
                address: Address::parse("11:22:33:44:55:66").unwrap(),
                name: "Paired phone".into(),
                powered: true,
            }]
        );
    }

    #[test]
    fn names_fall_back_and_power_is_tracked() {
        let mut o = Objects::new();
        obj(&mut o, "/org/bluez/hci0", ADAPTER_IFACE, adapter(false));
        let mut d = device("aa:bb:cc:dd:ee:ff", "\u{200B}\u{202E}", true, &[OPP_UUID]);
        d.insert("Name".into(), v("\u{202E}Real name".to_owned()));
        obj(&mut o, "/org/bluez/hci0/dev_a", DEVICE_IFACE, d);
        let mut d = device("AA:BB:CC:DD:EE:FE", "", true, &[OPP_UUID]);
        d.remove("Adapter");
        obj(&mut o, "/org/bluez/hci0/dev_b", DEVICE_IFACE, d);
        let devices = parse_managed_objects(&reply(o)).unwrap();
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "AA:BB:CC:DD:EE:FE");
        assert_eq!(devices[1].name, "Real name");
        assert!(devices.iter().all(|d| !d.powered));
    }

    #[test]
    fn replies_of_the_wrong_shape_are_refused_or_skipped() {
        let m = Message::new_signal("/", "a.b", "C")
            .unwrap()
            .append1("nope");
        assert_eq!(parse_managed_objects(&m), None);
        let m = Message::new_signal("/", "a.b", "C").unwrap();
        assert_eq!(parse_managed_objects(&m), None);
        let m = Message::new_signal("/", "a.b", "C")
            .unwrap()
            .append1(vec!["a".to_owned()]);
        assert_eq!(parse_managed_objects(&m), Some(vec![]));
        let mut wrong: HashMap<String, u32> = HashMap::new();
        wrong.insert("x".into(), 1);
        let m = Message::new_signal("/", "a.b", "C").unwrap().append1(wrong);
        assert_eq!(parse_managed_objects(&m), Some(vec![]));
    }

    #[test]
    fn huge_replies_are_bounded() {
        let mut o = Objects::new();
        obj(&mut o, "/org/bluez/hci0", ADAPTER_IFACE, adapter(true));
        for i in 0..300u32 {
            let a = format!("10:00:00:00:{:02X}:{:02X}", i >> 8, i & 0xff);
            obj(
                &mut o,
                &format!("/org/bluez/hci0/dev_{i}"),
                DEVICE_IFACE,
                device(&a, &format!("dev {i:04}"), true, &[OPP_UUID]),
            );
        }
        let devices = parse_managed_objects(&reply(o)).unwrap();
        assert_eq!(devices.len(), MAX_DEVICES);
        let mut sorted = devices.clone();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(devices, sorted);
    }

    /// `filler` empty objects, then one eligible device, in that order.
    fn padded(filler: usize) -> Message {
        use dbus::arg::IterAppend;
        use dbus::strings::Signature;
        let mut m = Message::new_signal("/", "org.example.Test", "Reply").unwrap();
        let mut ifaces = HashMap::new();
        ifaces.insert(
            DEVICE_IFACE.to_owned(),
            device("11:22:33:44:55:66", "Last", true, &[OPP_UUID]),
        );
        let empty: HashMap<String, PropMap> = HashMap::new();
        IterAppend::new(&mut m).append_dict(
            &Signature::new("o").unwrap(),
            &Signature::new("a{sa{sv}}").unwrap(),
            |dict| {
                for i in 0..filler {
                    dict.append_dict_entry(|e| {
                        e.append(Path::new(format!("/f/{i}")).unwrap());
                        e.append(&empty);
                    });
                }
                dict.append_dict_entry(|e| {
                    e.append(Path::new("/org/bluez/hci0/dev_last").unwrap());
                    e.append(&ifaces);
                });
            },
        );
        m
    }

    #[test]
    fn the_object_cap_holds() {
        assert_eq!(
            parse_managed_objects(&padded(MAX_OBJECTS - 1))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            parse_managed_objects(&padded(MAX_OBJECTS)).unwrap().len(),
            0
        );
    }

    /// Deterministic mutation sweep: a realistic reply, marshalled, with
    /// every byte flipped three ways. Whatever libdbus still accepts as a
    /// message -- any shape at all -- must parse without a panic into
    /// devices that keep every invariant.
    #[test]
    fn mutated_replies_never_panic() {
        let mut o = Objects::new();
        obj(&mut o, "/org/bluez/hci0", ADAPTER_IFACE, adapter(true));
        obj(
            &mut o,
            "/org/bluez/hci0/dev_1",
            DEVICE_IFACE,
            device("11:22:33:44:55:66", "Pekka\u{202E}", true, &[OPP_UUID]),
        );
        let mut d = device("11:22:33:44:55:67", "Anna", true, &[OPP_UUID]);
        d.insert("Name".into(), v("Anna's phone".to_owned()));
        obj(&mut o, "/org/bluez/hci0/dev_2", DEVICE_IFACE, d);
        let mut msg = reply(o);
        msg.set_serial(1);
        let mut bytes = Vec::new();
        msg.marshal(|b| {
            bytes.extend_from_slice(b);
            Ok::<(), ()>(())
        })
        .unwrap();
        assert_eq!(
            parse_managed_objects(&Message::demarshal(&bytes).unwrap())
                .unwrap()
                .len(),
            2
        );
        let mut accepted = 0usize;
        for i in 0..bytes.len() {
            for mask in [0x01u8, 0x20, 0xff] {
                let mut m = bytes.clone();
                m[i] ^= mask;
                let Ok(msg) = Message::demarshal(&m) else {
                    continue;
                };
                accepted += 1;
                for d in parse_managed_objects(&msg).unwrap_or_default() {
                    assert_eq!(Address::parse(&d.address.to_string()), Some(d.address));
                    assert!(!d.name.is_empty());
                    assert!(d.name.chars().count() <= MAX_ALIAS_CHARS);
                    assert!(!d.name.chars().any(text::is_forbidden));
                }
            }
        }
        // The sweep reached the parser, not just libdbus's validator.
        assert!(accepted > 100, "{accepted}");
    }
}
