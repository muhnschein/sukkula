//! The user's settings, as stored and as changed from the UI.
//!
//! Settings arrive from two places Sukkula does not control: a file on disk
//! (which may be from an older version, or edited by hand) and the UI. Both
//! go through [`Settings::validate`]. Unknown fields are refused, lengths
//! are capped, and URLs are held to the schemes each protocol can use.
//!
//! # A file that does not read
//!
//! The UI's settings are refused whole, and the user sees why. The file is
//! read by [`Settings::from_stored`], which fails closed, part by part:
//!
//! - A part -- the device name, a protocol's section, the logging switch --
//!   that reads and validates on its own is kept as it is. One bad field no
//!   longer throws away everything else: a mailbox URL an older build took
//!   and this one refuses does not cost the LocalSend PIN or Quick Share's
//!   Hidden ("One invalid or unknown field in settings.json silently
//!   resets every setting").
//! - A protocol section that does not is not reset to its defaults, which
//!   are the most permissive values there are (LocalSend without a PIN,
//!   Quick Share visible to everyone). The protocol is switched off and its
//!   options take their strictest values: no PIN, Hidden, no BLE nudge, the
//!   default servers. Whatever the file meant for it, the phone now does
//!   less, not more.
//! - A key this build does not know may be a restriction from a newer one;
//!   what it restricted cannot be known, so every protocol is switched off.
//!   A key given twice cannot be read one way only, and a file that is not
//!   a JSON object cannot be read at all: both give
//!   [`Settings::locked_down`].
//!
//! The engine tells the UI (`recovered` in the `settings` event), and
//! leaves the file as it is until the user saves settings again.

use serde::de::{self, DeserializeOwned, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::limits::{MAX_ALIAS_CHARS, MAX_PIN_CHARS};
use crate::text;

/// Longest custom server URL accepted.
pub const MAX_URL_BYTES: usize = 256;

/// The settings file's name in the data directory.
pub const SETTINGS_FILE: &str = "settings.json";

/// Largest settings file read.
pub const MAX_SETTINGS_BYTES: usize = 16 * 1024;

/// Every setting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Settings {
    /// The name peers see (F-C7). Empty means the device model.
    pub device_name: String,
    /// LocalSend.
    pub localsend: LocalSendSettings,
    /// Quick Share.
    pub quickshare: QuickShareSettings,
    /// Magic Wormhole.
    pub wormhole: WormholeSettings,
    /// Bluetooth.
    pub bluetooth: BluetoothSettings,
    /// Debug logging (S9). Off by default, and never file names or text at
    /// info level even when on.
    pub logging: bool,
}

/// LocalSend settings.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LocalSendSettings {
    /// Receive over LocalSend (F-C1).
    pub enabled: bool,
    /// A PIN senders must supply (F-LS4). Off by default.
    pub pin: Option<String>,
}

impl std::fmt::Debug for LocalSendSettings {
    /// S9: the PIN is a shared secret, so a `{:?}` of the settings -- in a
    /// log line, a panic message -- says whether there is one, not what it
    /// is.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalSendSettings")
            .field("enabled", &self.enabled)
            .field("pin", &self.pin.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Quick Share visibility (F-QS4). Contacts-only needs Google account keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Not advertised; nobody can find this phone.
    Hidden,
    /// Advertised to everyone nearby while receiving is on.
    #[default]
    Everyone,
}

/// Quick Share settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QuickShareSettings {
    /// Receive over Quick Share (F-C1).
    pub enabled: bool,
    /// Who can see this phone.
    pub visibility: Visibility,
    /// Advertise over BLE so Android phones look for us (F-QS2).
    pub ble_nudge: bool,
}

/// Magic Wormhole settings (F-MW4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WormholeSettings {
    /// Send and receive with Magic Wormhole (F-C1). A file from before this
    /// setting has none, and reads as on.
    // CONTRACT: new field (F-C1; the one change to the settings' shape).
    pub enabled: bool,
    /// A mailbox server instead of the default, `ws://` or `wss://`.
    pub mailbox_url: Option<String>,
    /// A transit relay instead of the default, `tcp://host:port`.
    pub relay_url: Option<String>,
}

/// Bluetooth settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BluetoothSettings {
    /// Offer Bluetooth as a send target.
    pub enabled: bool,
}

impl Default for LocalSendSettings {
    fn default() -> Self {
        LocalSendSettings {
            enabled: true,
            pin: None,
        }
    }
}

impl Default for QuickShareSettings {
    fn default() -> Self {
        QuickShareSettings {
            enabled: true,
            visibility: Visibility::Everyone,
            ble_nudge: true,
        }
    }
}

impl Default for WormholeSettings {
    fn default() -> Self {
        WormholeSettings {
            enabled: true,
            mailbox_url: None,
            relay_url: None,
        }
    }
}

impl Default for BluetoothSettings {
    fn default() -> Self {
        BluetoothSettings { enabled: true }
    }
}

impl LocalSendSettings {
    /// Off, and no PIN kept.
    fn locked_down() -> Self {
        LocalSendSettings {
            enabled: false,
            pin: None,
        }
    }
}

impl QuickShareSettings {
    /// Off, hidden, and silent over BLE.
    fn locked_down() -> Self {
        QuickShareSettings {
            enabled: false,
            visibility: Visibility::Hidden,
            ble_nudge: false,
        }
    }
}

impl WormholeSettings {
    /// Off, and no server of anyone's choosing.
    fn locked_down() -> Self {
        WormholeSettings {
            enabled: false,
            mailbox_url: None,
            relay_url: None,
        }
    }
}

impl BluetoothSettings {
    /// Off.
    fn locked_down() -> Self {
        BluetoothSettings { enabled: false }
    }
}

/// What [`Settings::from_stored`] made of a settings file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    /// The settings to run with.
    pub settings: Settings,
    /// The parts of the file that could not be used as they were, by name:
    /// `device_name`, `localsend`, `quickshare`, `wormhole`, `bluetooth`,
    /// `logging`, `unknown` for a key this build does not know, `file` when
    /// nothing could be read. Empty when the file was used whole. Fixed
    /// names, never the file's content: safe to log (S9).
    pub unusable: Vec<&'static str>,
}

/// A setting that cannot be used.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    /// The PIN is empty, too long, or not printable ASCII.
    #[error("the LocalSend PIN must be 1 to {MAX_PIN_CHARS} letters or digits")]
    BadPin,
    /// A server URL that is too long or has the wrong scheme.
    #[error("the {0} URL is not usable")]
    BadUrl(&'static str),
}

impl Settings {
    /// Normalises and checks the settings.
    ///
    /// # Errors
    ///
    /// The first setting that cannot be used.
    pub fn validate(mut self) -> Result<Settings, ConfigError> {
        self.device_name = text::display(&self.device_name, MAX_ALIAS_CHARS);
        if let Some(pin) = self.localsend.pin.take() {
            let pin = pin.trim().to_owned();
            if !pin.is_empty() {
                let ok = pin.chars().count() <= MAX_PIN_CHARS
                    && pin.chars().all(|c| c.is_ascii_alphanumeric());
                if !ok {
                    return Err(ConfigError::BadPin);
                }
                self.localsend.pin = Some(pin);
            }
        }
        self.wormhole.mailbox_url = check_url(
            self.wormhole.mailbox_url.take(),
            &["ws://", "wss://"],
            "mailbox",
        )?;
        self.wormhole.relay_url = check_url(self.wormhole.relay_url.take(), &["tcp://"], "relay")?;
        Ok(self)
    }

    /// The strictest settings there are: every protocol off, Quick Share
    /// hidden and silent, no PIN, no custom server, logging off. What a
    /// settings file that cannot be read at all becomes.
    #[must_use]
    pub fn locked_down() -> Settings {
        Settings {
            device_name: String::new(),
            localsend: LocalSendSettings::locked_down(),
            quickshare: QuickShareSettings::locked_down(),
            wormhole: WormholeSettings::locked_down(),
            bluetooth: BluetoothSettings::locked_down(),
            logging: false,
        }
    }

    /// Reads a settings file, failing closed: see the module docs, "A file
    /// that does not read". A file that parses and validates whole is used
    /// as it is.
    #[must_use]
    pub fn from_stored(bytes: &[u8]) -> Stored {
        if let Ok(whole) = serde_json::from_slice::<Settings>(bytes)
            && let Ok(settings) = whole.validate()
        {
            return Stored {
                settings,
                unusable: Vec::new(),
            };
        }
        let Ok(Unique(Value::Object(map))) = serde_json::from_slice::<Unique>(bytes) else {
            return Stored {
                settings: Settings::locked_down(),
                unusable: vec!["file"],
            };
        };
        let mut s = Settings::default();
        let mut unusable = Vec::new();
        for (key, value) in map {
            let (part, used) = match key.as_str() {
                "device_name" => ("device_name", s.adopt(value, |s, v| s.device_name = v)),
                "localsend" => ("localsend", s.adopt(value, |s, v| s.localsend = v)),
                "quickshare" => ("quickshare", s.adopt(value, |s, v| s.quickshare = v)),
                "wormhole" => ("wormhole", s.adopt(value, |s, v| s.wormhole = v)),
                "bluetooth" => ("bluetooth", s.adopt(value, |s, v| s.bluetooth = v)),
                "logging" => ("logging", s.adopt(value, |s, v| s.logging = v)),
                _ => ("unknown", false),
            };
            if used {
                continue;
            }
            match part {
                "localsend" => s.localsend = LocalSendSettings::locked_down(),
                "quickshare" => s.quickshare = QuickShareSettings::locked_down(),
                "wormhole" => s.wormhole = WormholeSettings::locked_down(),
                "bluetooth" => s.bluetooth = BluetoothSettings::locked_down(),
                // The name falls back to the model and logging stays off:
                // neither lets anything in.
                _ => {}
            }
            if !unusable.contains(&part) {
                unusable.push(part);
            }
        }
        if unusable.contains(&"unknown") {
            s.localsend.enabled = false;
            s.quickshare.enabled = false;
            s.wormhole.enabled = false;
            s.bluetooth.enabled = false;
        }
        Stored {
            settings: s,
            unusable,
        }
    }

    /// Takes `value` as the part `put` sets, if it parses as that part and
    /// the whole still validates. False, and nothing changed, otherwise.
    fn adopt<T: DeserializeOwned>(
        &mut self,
        value: Value,
        put: impl FnOnce(&mut Settings, T),
    ) -> bool {
        let Ok(part) = serde_json::from_value::<T>(value) else {
            return false;
        };
        let mut next = self.clone();
        put(&mut next, part);
        match next.validate() {
            Ok(valid) => {
                *self = valid;
                true
            }
            Err(_) => false,
        }
    }

    /// The name to show peers: the setting, else `model`, else "Sailfish".
    #[must_use]
    pub fn effective_device_name(&self, model: &str) -> String {
        let name = text::display(&self.device_name, MAX_ALIAS_CHARS);
        if !name.is_empty() {
            return name;
        }
        let model = text::display(model, MAX_ALIAS_CHARS);
        if model.is_empty() {
            "Sailfish".to_owned()
        } else {
            model
        }
    }
}

/// A JSON value in which no object has a key twice, at any depth.
/// serde_json's own `Value` keeps the last of two, and a file that says two
/// things about one setting cannot be read one way only. Nesting is bounded
/// by serde_json's recursion limit, and size by the file's.
struct Unique(Value);

impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(UniqueVisitor)
    }
}

struct UniqueVisitor;

impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = Unique;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Unique, E> {
        Ok(Unique(Value::Bool(v)))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Unique, E> {
        Ok(Unique(Value::from(v)))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Unique, E> {
        Ok(Unique(Value::from(v)))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
        serde_json::Number::from_f64(v)
            .map(|n| Unique(Value::Number(n)))
            .ok_or_else(|| E::custom("not a JSON number"))
    }

    fn visit_str<E>(self, v: &str) -> Result<Unique, E> {
        Ok(Unique(Value::String(v.to_owned())))
    }

    fn visit_string<E>(self, v: String) -> Result<Unique, E> {
        Ok(Unique(Value::String(v)))
    }

    fn visit_unit<E>(self) -> Result<Unique, E> {
        Ok(Unique(Value::Null))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Unique, A::Error> {
        let mut items = Vec::new();
        while let Some(Unique(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Unique(Value::Array(items)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Unique, A::Error> {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if out.contains_key(&key) {
                return Err(de::Error::custom("a key given twice"));
            }
            let Unique(value) = map.next_value()?;
            out.insert(key, value);
        }
        Ok(Unique(Value::Object(out)))
    }
}

fn check_url(
    url: Option<String>,
    schemes: &[&str],
    what: &'static str,
) -> Result<Option<String>, ConfigError> {
    let Some(url) = url.map(|u| u.trim().to_owned()).filter(|u| !u.is_empty()) else {
        return Ok(None);
    };
    let has_scheme = schemes
        .iter()
        .any(|s| url.len() > s.len() && url.to_ascii_lowercase().starts_with(s));
    let printable = url.bytes().all(|b| b.is_ascii_graphic());
    // A server is a host and a port, and for a mailbox a path: never
    // credentials (`user:pass@`), which would sit in the settings file and
    // go to whoever the host resolves to, and never a query or fragment,
    // which the protocols have no use for and only hide what the URL is.
    let authority = schemes
        .iter()
        .find(|s| url.to_ascii_lowercase().starts_with(*s))
        .and_then(|s| url.get(s.len()..))
        .map(|rest| rest.split('/').next().unwrap_or(""))
        .unwrap_or("");
    let plain = !authority.is_empty() && !authority.contains('@') && !url.contains(['?', '#']);
    if url.len() > MAX_URL_BYTES || !has_scheme || !printable || !plain {
        return Err(ConfigError::BadUrl(what));
    }
    Ok(Some(url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_private_by_default() {
        let s = Settings::default().validate().unwrap();
        assert_eq!(s.localsend.pin, None);
        assert!(!s.logging);
    }

    #[test]
    fn unknown_fields_are_refused() {
        assert!(serde_json::from_str::<Settings>(r#"{"auto_accept": true}"#).is_err());
        assert!(
            serde_json::from_str::<Settings>(r#"{"localsend": {"enabled": true, "x": 1}}"#)
                .is_err()
        );
        let partial: Settings = serde_json::from_str(r#"{"logging": true}"#).unwrap();
        assert!(partial.logging);
        assert!(partial.localsend.enabled);
    }

    #[test]
    fn pins_and_urls_are_checked() {
        let mut s = Settings::default();
        s.localsend.pin = Some(" 1234 ".into());
        assert_eq!(
            s.clone().validate().unwrap().localsend.pin.as_deref(),
            Some("1234")
        );
        s.localsend.pin = Some("12 34".into());
        assert_eq!(s.clone().validate(), Err(ConfigError::BadPin));
        s.localsend.pin = Some(String::new());
        assert_eq!(s.clone().validate().unwrap().localsend.pin, None);
        s.localsend.pin = None;
        s.wormhole.mailbox_url = Some("http://evil".into());
        assert_eq!(s.clone().validate(), Err(ConfigError::BadUrl("mailbox")));
        s.wormhole.mailbox_url = Some("wss://relay.example/v1".into());
        s.wormhole.relay_url = Some("tcp://relay.example:4001".into());
        assert!(s.clone().validate().is_ok());
        assert!(s.wormhole.enabled, "on unless switched off");
        for bad in [
            "wss://user:pass@relay.example/v1",
            "wss://relay.example/v1?token=x",
            "wss://relay.example/v1#frag",
            "wss:///v1",
        ] {
            s.wormhole.mailbox_url = Some(bad.into());
            assert_eq!(
                s.clone().validate(),
                Err(ConfigError::BadUrl("mailbox")),
                "{bad}"
            );
        }
        s.wormhole.mailbox_url = None;
        s.wormhole.relay_url = Some("tcp://user@relay.example:4001".into());
        assert_eq!(s.clone().validate(), Err(ConfigError::BadUrl("relay")));
        s.wormhole.relay_url = Some("tcp://a b".into());
        assert_eq!(s.validate(), Err(ConfigError::BadUrl("relay")));
    }

    #[test]
    fn the_device_name_falls_back_to_the_model() {
        let mut s = Settings::default();
        assert_eq!(s.effective_device_name("Jolla Phone"), "Jolla Phone");
        assert_eq!(s.effective_device_name("\u{202E}"), "Sailfish");
        s.device_name = "Pekka\u{200B}".into();
        assert_eq!(s.effective_device_name("Jolla Phone"), "Pekka");
    }

    #[test]
    fn validation_is_idempotent_and_capped() {
        let mut s = Settings {
            device_name: format!("  \u{202E}{}\u{2066} ", "x".repeat(500)),
            ..Settings::default()
        };
        s.localsend.pin = Some("\tABC123 ".into());
        s.wormhole.mailbox_url = Some(" WSS://relay.example/v1 ".into());
        let once = s.validate().unwrap();
        assert_eq!(once.device_name.chars().count(), MAX_ALIAS_CHARS);
        assert!(!once.device_name.contains('\u{202E}'));
        assert_eq!(once.localsend.pin.as_deref(), Some("ABC123"));
        assert_eq!(
            once.wormhole.mailbox_url.as_deref(),
            Some("WSS://relay.example/v1")
        );
        assert_eq!(once.clone().validate().unwrap(), once);
        // What is saved reads back as what was validated.
        let json = serde_json::to_string(&once).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.validate().unwrap(), once);
    }

    #[test]
    fn pin_and_url_edges() {
        let mut s = Settings::default();
        s.localsend.pin = Some("1".repeat(MAX_PIN_CHARS));
        assert!(s.clone().validate().is_ok());
        s.localsend.pin = Some("1".repeat(MAX_PIN_CHARS + 1));
        assert_eq!(s.clone().validate(), Err(ConfigError::BadPin));
        s.localsend.pin = Some("١٢٣٤".into()); // Arabic-Indic digits
        assert_eq!(s.clone().validate(), Err(ConfigError::BadPin));
        s.localsend.pin = None;
        for bad in [
            "ws://",
            "wss://",
            "ws:/x",
            "file:///etc/passwd",
            "ws://a\u{0}b",
            "ws://ä",
        ] {
            s.wormhole.mailbox_url = Some(bad.into());
            assert_eq!(
                s.clone().validate(),
                Err(ConfigError::BadUrl("mailbox")),
                "{bad}"
            );
        }
        s.wormhole.mailbox_url = Some(format!("ws://{}", "a".repeat(MAX_URL_BYTES)));
        assert_eq!(s.clone().validate(), Err(ConfigError::BadUrl("mailbox")));
        s.wormhole.mailbox_url = Some("   ".into());
        assert_eq!(s.clone().validate().unwrap().wormhole.mailbox_url, None);
        s.wormhole.relay_url = Some("ws://relay.example:4001".into());
        assert_eq!(s.clone().validate(), Err(ConfigError::BadUrl("relay")));
        assert!(!ConfigError::BadPin.to_string().is_empty());
        assert!(ConfigError::BadUrl("relay").to_string().contains("relay"));
    }

    #[test]
    fn the_pin_never_shows_in_debug_output() {
        let mut s = Settings::default();
        s.localsend.pin = Some("48151623".into());
        let shown = format!("{s:?}");
        assert!(!shown.contains("48151623"), "{shown}");
        assert!(shown.contains("<redacted>"));
        s.localsend.pin = None;
        assert!(format!("{s:?}").contains("pin: None"));
    }

    /// The file the tests below damage: every choice a user can make
    /// against the defaults, to be kept or failed closed.
    const CAREFUL: &str = r#"{
        "device_name": "Pekka",
        "localsend": {"enabled": true, "pin": "4711"},
        "quickshare": {"enabled": true, "visibility": "hidden", "ble_nudge": false},
        "wormhole": {"enabled": true, "mailbox_url": "wss://m.example/v1", "relay_url": "tcp://r.example:4001"},
        "bluetooth": {"enabled": false},
        "logging": false
    }"#;

    fn careful() -> Settings {
        serde_json::from_str::<Settings>(CAREFUL)
            .unwrap()
            .validate()
            .unwrap()
    }

    /// `CAREFUL` with the part `key` replaced by `value` (or removed).
    fn damaged(key: &str, value: Option<Value>) -> Vec<u8> {
        let mut v: Value = serde_json::from_str(CAREFUL).unwrap();
        let map = v.as_object_mut().unwrap();
        match value {
            Some(value) => {
                map.insert(key.to_owned(), value);
            }
            None => {
                map.remove(key);
            }
        }
        serde_json::to_vec(&v).unwrap()
    }

    #[test]
    fn a_whole_file_is_used_as_it_is() {
        let stored = Settings::from_stored(CAREFUL.as_bytes());
        assert_eq!(stored.settings, careful());
        assert!(stored.unusable.is_empty());
        // Parts left out take their defaults, as they always did, and a file
        // from before `wormhole.enabled` reads as on.
        let stored = Settings::from_stored(br#"{"logging":true,"wormhole":{"mailbox_url":null}}"#);
        assert!(stored.unusable.is_empty());
        assert!(stored.settings.logging);
        assert!(stored.settings.wormhole.enabled);
        assert_eq!(stored.settings.localsend, LocalSendSettings::default());
        assert_eq!(Settings::from_stored(b"{}").settings, Settings::default());
    }

    #[test]
    fn one_bad_part_costs_only_that_part() {
        // The review's case: a mailbox URL an earlier build took.
        let bytes = damaged(
            "wormhole",
            Some(serde_json::json!({"mailbox_url": "wss://relay.example/v1?token=x"})),
        );
        let stored = Settings::from_stored(&bytes);
        assert_eq!(stored.unusable, vec!["wormhole"]);
        let s = stored.settings;
        assert_eq!(s.localsend.pin.as_deref(), Some("4711"), "the PIN survives");
        assert!(s.localsend.enabled);
        assert_eq!(
            s.quickshare.visibility,
            Visibility::Hidden,
            "so does Hidden"
        );
        assert!(s.quickshare.enabled);
        assert_eq!(s.device_name, "Pekka");
        // And the part that did not read is off, with no server of anyone's.
        assert!(!s.wormhole.enabled);
        assert_eq!(s.wormhole.mailbox_url, None);
        assert_eq!(s.wormhole.relay_url, None);
    }

    #[test]
    fn a_bad_section_is_switched_off_not_reset_to_its_defaults() {
        let bad_pin = damaged(
            "localsend",
            Some(serde_json::json!({"enabled": true, "pin": "12 34"})),
        );
        let s = Settings::from_stored(&bad_pin).settings;
        assert_eq!(s.localsend, LocalSendSettings::locked_down());
        assert_eq!(s.quickshare, careful().quickshare);
        let contacts = damaged(
            "quickshare",
            Some(serde_json::json!({"enabled": true, "visibility": "contacts"})),
        );
        let s = Settings::from_stored(&contacts).settings;
        assert_eq!(s.quickshare, QuickShareSettings::locked_down());
        assert_eq!(s.quickshare.visibility, Visibility::Hidden);
        assert_eq!(s.localsend, careful().localsend);
        let from_a_newer_build = damaged(
            "localsend",
            Some(serde_json::json!({"enabled": true, "pin": "4711", "allow_only": ["x"]})),
        );
        let stored = Settings::from_stored(&from_a_newer_build);
        assert_eq!(stored.unusable, vec!["localsend"]);
        assert!(!stored.settings.localsend.enabled);
    }

    #[test]
    fn an_unknown_key_switches_every_protocol_off() {
        let mut v: Value = serde_json::from_str(CAREFUL).unwrap();
        v["contacts_only"] = Value::Bool(true);
        let stored = Settings::from_stored(&serde_json::to_vec(&v).unwrap());
        assert_eq!(stored.unusable, vec!["unknown"]);
        let s = stored.settings;
        assert!(!s.localsend.enabled && !s.quickshare.enabled);
        assert!(!s.wormhole.enabled && !s.bluetooth.enabled);
        // Everything else is kept, for when the user switches them back on.
        assert_eq!(s.localsend.pin.as_deref(), Some("4711"));
        assert_eq!(s.quickshare.visibility, Visibility::Hidden);
        assert_eq!(s.device_name, "Pekka");
    }

    #[test]
    fn a_file_that_cannot_be_read_one_way_is_locked_down() {
        for bytes in [
            &b""[..],
            b"nonsense",
            b"7",
            b"null",
            b"{\"localsend\":",
            br#"{"logging":false,"logging":true}"#,
            br#"{"localsend":{"pin":"4711","pin":null}}"#,
            br#"{"wormhole":{"mailbox_url":"ws://a/v1"},"x":{"y":{"z":1,"z":2}}}"#,
            b"\xef\xbb\xbf{}",
        ] {
            let stored = Settings::from_stored(bytes);
            assert_eq!(stored.settings, Settings::locked_down(), "{bytes:?}");
            assert_eq!(stored.unusable, vec!["file"], "{bytes:?}");
        }
        let s = Settings::locked_down();
        assert!(!s.localsend.enabled && !s.quickshare.enabled);
        assert!(!s.wormhole.enabled && !s.bluetooth.enabled && !s.logging);
        assert_eq!(s.quickshare.visibility, Visibility::Hidden);
        assert!(!s.quickshare.ble_nudge);
        assert_eq!(s.clone().validate().unwrap(), s);
    }

    /// Every part of the careful file, damaged every way: the result is never
    /// more permissive than the file was.
    #[test]
    fn damage_never_opens_anything_up() {
        let bad = [
            Value::Null,
            Value::Bool(true),
            serde_json::json!(1),
            serde_json::json!(-1.5),
            serde_json::json!("everyone"),
            serde_json::json!([1]),
            serde_json::json!({"enabled": "yes"}),
            serde_json::json!({"enabled": true, "surprise": 1}),
            serde_json::json!({"enabled": true, "pin": 4711}),
            serde_json::json!({"enabled": true, "pin": "12 34"}),
            serde_json::json!({"enabled": true, "visibility": "contacts"}),
            serde_json::json!({"enabled": true, "ble_nudge": "on"}),
            serde_json::json!({"enabled": true, "mailbox_url": "http://evil/v1"}),
            serde_json::json!({"enabled": true, "relay_url": "tcp://u@evil:1"}),
        ];
        let keys = [
            "device_name",
            "localsend",
            "quickshare",
            "wormhole",
            "bluetooth",
            "logging",
            "extra",
        ];
        let base = careful();
        let mut damaged_files = 0;
        for key in keys {
            for value in &bad {
                let bytes = damaged(key, Some(value.clone()));
                let whole = serde_json::from_slice::<Settings>(&bytes)
                    .ok()
                    .and_then(|s| s.validate().ok());
                if whole.is_some() {
                    // Not damage: a file that says something else.
                    continue;
                }
                damaged_files += 1;
                let s = Settings::from_stored(&bytes).settings;
                let at = format!("{key} = {value}");
                if s.localsend.enabled {
                    assert_eq!(s.localsend.pin, base.localsend.pin, "{at}");
                }
                if s.quickshare.enabled {
                    assert_eq!(s.quickshare.visibility, Visibility::Hidden, "{at}");
                    assert!(!s.quickshare.ble_nudge, "{at}");
                }
                if s.wormhole.enabled {
                    assert_eq!(s.wormhole.mailbox_url, base.wormhole.mailbox_url, "{at}");
                    assert_eq!(s.wormhole.relay_url, base.wormhole.relay_url, "{at}");
                }
                assert!(!s.bluetooth.enabled, "{at}");
                assert!(!s.logging, "{at}");
                assert_eq!(s.clone().validate().unwrap(), s, "{at}");
            }
        }
        assert!(damaged_files > 60, "{damaged_files}");
    }

    #[test]
    fn the_file_format_is_strict() {
        assert!(
            serde_json::from_str::<Settings>(r#"{"quickshare":{"visibility":"contacts"}}"#)
                .is_err()
        );
        let s: Settings =
            serde_json::from_str(r#"{"quickshare":{"visibility":"hidden"},"bluetooth":{}}"#)
                .unwrap();
        assert_eq!(s.quickshare.visibility, Visibility::Hidden);
        assert!(s.bluetooth.enabled);
        assert!(serde_json::from_str::<Settings>(r#"{"wormhole":{"relay":"x"}}"#).is_err());
        assert!(serde_json::from_str::<Settings>(r#"{"bluetooth":{"enabled":1}}"#).is_err());
        assert!(serde_json::from_str::<Settings>("7").is_err());
        // serde's derive also takes a struct as a positional array. That is
        // the same fields through the same `validate`, not a way around it;
        // one element too many is still refused.
        let positional: Settings = serde_json::from_str("[]").unwrap();
        assert_eq!(positional, Settings::default());
        assert!(serde_json::from_str::<Settings>(r#"["", {}, {}, {}, {}, false, 1]"#).is_err());
        assert_eq!(
            serde_json::to_string(&Visibility::Everyone).unwrap(),
            r#""everyone""#
        );
    }
}
