//! The user's settings, as stored and as changed from the UI.
//!
//! Settings arrive from two places Sukkula does not control: a file on disk
//! (which may be from an older version, or edited by hand) and the UI. Both
//! go through [`Settings::validate`]. Unknown fields are refused, lengths
//! are capped, and URLs are held to the schemes each protocol can use.

use serde::{Deserialize, Serialize};
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LocalSendSettings {
    /// Receive over LocalSend (F-C1).
    pub enabled: bool,
    /// A PIN senders must supply (F-LS4). Off by default.
    pub pin: Option<String>,
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WormholeSettings {
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

impl Default for BluetoothSettings {
    fn default() -> Self {
        BluetoothSettings { enabled: true }
    }
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
    if url.len() > MAX_URL_BYTES || !has_scheme || !printable {
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
}
