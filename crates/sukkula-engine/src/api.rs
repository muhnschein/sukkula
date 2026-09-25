//! The engine's JSON interface: commands in, events out.
//!
//! This is the whole contract between the Rust engine and the Qt shell.
//! `sukkula-ffi` moves these as JSON strings across the C ABI; the QML side
//! (`qml/engine/Engine.qml`) is written against the shapes below and nothing
//! else. `docs/FFI.md` shows every message with an example.
//!
//! Commands come from the UI, which is ours, but they are parsed as if they
//! were not: at most [`MAX_MESSAGE_BYTES`], unknown fields refused, the
//! version checked. Every command is answered by exactly one
//! [`Event::Reply`] carrying its `id`.
//!
//! Events are built by the engine from sanitised values only. Every string
//! in an event that a peer influenced has been through `sukkula_core::text`
//! or `sukkula_core::name`, and QML still shows them as plain text.

use serde::{Deserialize, Serialize};
use sukkula_core::Protocol;
use sukkula_core::config::Settings;
use sukkula_core::consent::Closed;
use sukkula_core::limits::MAX_MESSAGE_BYTES;
use sukkula_core::text;
use thiserror::Error;

/// The version of this interface. Bumped on any incompatible change.
pub const API_VERSION: u32 = 1;

/// Identifies a transfer, in either direction, for the life of the engine.
pub type TransferId = u64;

/// Identifies an offer waiting for the user.
pub type OfferId = u64;

/// Identifies a command, chosen by the UI, echoed in its [`Event::Reply`].
pub type RequestId = u64;

/// Most files an [`OfferView`] lists by name; the rest are counted in
/// `more_files`. Keeps an event bounded when an offer carries 500 files.
pub const MAX_LISTED_FILES: usize = 50;

/// Most commands the engine holds at once, from being taken until their
/// [`Event::Reply`] has been handed to the UI. One more is refused without
/// a reply (`SUKKULA_ERR_BUSY` at the C ABI), so a UI stuck in a loop costs
/// bounded memory rather than an ever longer queue.
// CONTRACT: new (additive), with SUKKULA_ERR_BUSY in sukkula.h.
pub const MAX_IN_FLIGHT_COMMANDS: usize = 64;

/// Longest [`ErrorInfo::message`], in characters. Messages are for logs;
/// the cap keeps a serde error that quotes a 60 KiB command from making
/// its reply 60 KiB long.
// CONTRACT: new (additive).
pub const MAX_ERROR_MESSAGE_CHARS: usize = 256;

/// What the shell passes to `sukkula_start`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartConfig {
    /// Must be [`API_VERSION`].
    pub v: u32,
    /// The app's private data directory (settings, TLS key). Absolute.
    pub data_dir: String,
    /// Where received files go, e.g. `~/Downloads/Sukkula`. Absolute.
    pub download_dir: String,
    /// The device model, for the default device name (F-C7). When absent the
    /// engine reads `/etc/hw-release`.
    ///
    /// Left out when absent, as `allow_loopback` is when false: written as
    /// `null` and `false`, a configuration that came in just under
    /// [`MAX_MESSAGE_BYTES`] without them went out over it, and was refused
    /// by the parser that had accepted it (found by the `start_config` fuzz
    /// target).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_model: Option<String>,
    /// Answer peers on loopback. Tests only; the shell never sets it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_loopback: bool,
}

/// One command from the UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandEnvelope {
    /// Must be [`API_VERSION`].
    pub v: u32,
    /// Echoed in the [`Event::Reply`].
    pub id: RequestId,
    /// What to do.
    pub cmd: Command,
}

/// What the UI can ask for.
///
/// Every command refuses a key it does not have, those without fields
/// included: `{"type":"get_settings","auto_accept":true}` is malformed.
// The variants without fields read through `no_fields`: serde's derive
// reads a unit variant of an internally tagged enum by draining and
// ignoring every other key, whatever `deny_unknown_fields` says, so that
// command was taken as a `get_settings` ("deny_unknown_fields is not
// enforced for unit variants"). The Rust shape and the JSON shape are what
// they were.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    /// Turns every enabled receiver on or off together (F-C1).
    SetReceiving {
        /// On or off.
        on: bool,
    },
    /// Replaces the settings. They are validated and saved, and receivers
    /// that are running restart with them.
    SetSettings {
        /// The new settings.
        settings: Settings,
    },
    /// Asks for an [`Event::Settings`].
    #[serde(deserialize_with = "no_fields")]
    GetSettings,
    /// Starts looking for LocalSend and Quick Share peers to send to.
    #[serde(deserialize_with = "no_fields")]
    StartDiscovery,
    /// Stops looking.
    #[serde(deserialize_with = "no_fields")]
    StopDiscovery,
    /// Answers a pending offer (F-C2).
    Answer {
        /// From [`Event::OfferPending`].
        offer: OfferId,
        /// Receive it or not.
        accept: bool,
    },
    /// Sends files and/or text.
    Send {
        /// Where to.
        target: SendTarget,
        /// What. At least one item.
        items: Vec<SendItem>,
    },
    /// Receives with a wormhole code (F-MW2). The offer then goes through
    /// consent like any other.
    ReceiveWormhole {
        /// The code the sender's screen shows, e.g. `7-guitarist-revenge`.
        code: String,
    },
    /// Cancels a transfer in either direction (F-C5).
    Cancel {
        /// From [`Event::TransferStarted`].
        transfer: TransferId,
    },
    /// Asks for an [`Event::BluetoothDevices`] listing paired devices that
    /// accept Object Push.
    #[serde(deserialize_with = "no_fields")]
    ListBluetoothDevices,
}

/// Where a [`Command::Send`] goes. As with [`Command`], a key a target
/// does not have is refused, [`SendTarget::Wormhole`]'s included.
// `Wormhole` reads through `no_fields`, as `Command`'s variants without
// fields do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "snake_case", deny_unknown_fields)]
pub enum SendTarget {
    /// A LocalSend peer from [`Event::PeerFound`].
    LocalSend {
        /// Its [`Peer::id`].
        peer: String,
    },
    /// A Quick Share peer from [`Event::PeerFound`].
    QuickShare {
        /// Its [`Peer::id`].
        peer: String,
    },
    /// Magic Wormhole: the engine allocates a code and reports it in
    /// [`Event::WormholeCode`] (F-MW1). One file, or one text.
    #[serde(deserialize_with = "no_fields")]
    Wormhole,
    /// A paired Bluetooth device from [`Event::BluetoothDevices`] (F-BT1).
    Bluetooth {
        /// Its address, `AA:BB:CC:DD:EE:FF`.
        address: String,
    },
}

/// One thing to send.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SendItem {
    /// A file, by absolute path, from the picker or the Share menu.
    File {
        /// Absolute path to a regular file.
        path: String,
    },
    /// A text.
    Text {
        /// At most [`MAX_MESSAGE_BYTES`].
        text: String,
    },
}

/// Everything the engine tells the UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The engine could not start. The only event a failed `sukkula_start`
    /// emits, and it is emitted before `sukkula_start` returns `NULL`.
    Fatal {
        /// Why.
        error: ErrorInfo,
    },
    /// The first event after start.
    Started {
        /// The engine's version.
        version: String,
        /// [`API_VERSION`].
        api: u32,
        /// The protocols this build contains.
        protocols: Vec<Protocol>,
    },
    /// The answer to one command.
    Reply {
        /// The command's id.
        id: RequestId,
        /// Whether it was carried out (or, for long work, started).
        ok: bool,
        /// Why not.
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<ErrorInfo>,
        /// The transfer a `send` or `receive_wormhole` started.
        #[serde(skip_serializing_if = "Option::is_none")]
        transfer: Option<TransferId>,
    },
    /// The current settings, after start, `get_settings` and `set_settings`.
    Settings {
        /// The settings.
        settings: Settings,
        /// The name peers actually see.
        effective_device_name: String,
        /// The settings file could not be used as it was saved -- edited by
        /// hand, from another version, damaged -- and what could not be
        /// read was switched off rather than reset to its defaults
        /// (`sukkula_core::config`, "A file that does not read"). Until the
        /// next `set_settings`. Absent when false.
        // CONTRACT: new field (additive), for the review's "one invalid
        // field in settings.json silently resets every setting".
        #[serde(default, skip_serializing_if = "is_false")]
        recovered: bool,
    },
    /// Whether receiving is on, and how each protocol is doing (F-C1).
    Receiving {
        /// The switch.
        on: bool,
        /// One entry per protocol in this build.
        protocols: Vec<ProtocolStatus>,
    },
    /// A peer to send to appeared or changed.
    PeerFound {
        /// The peer.
        peer: Peer,
    },
    /// A peer went away.
    PeerLost {
        /// Its [`Peer::id`].
        peer: String,
    },
    /// Ask the user about an offer (F-C2).
    OfferPending {
        /// What to show.
        offer: OfferView,
    },
    /// Take an offer off the screen.
    OfferClosed {
        /// The offer.
        offer: OfferId,
        /// Why.
        reason: Closed,
    },
    /// A transfer started (F-C5).
    TransferStarted {
        /// The transfer.
        transfer: TransferView,
    },
    /// Progress, at most a few times a second per transfer.
    TransferProgress {
        /// The transfer.
        transfer: TransferId,
        /// Bytes so far.
        bytes: u64,
        /// Bytes in all.
        total: u64,
    },
    /// A transfer ended.
    TransferFinished {
        /// The transfer.
        transfer: TransferId,
        /// How.
        outcome: Outcome,
        /// For a receive: the names the files were saved under, in the
        /// download directory. Names, never paths from a peer.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        saved: Vec<String>,
    },
    /// Text arrived (F-C4). Shown as plain text with a Copy button, never
    /// opened.
    TextReceived {
        /// The transfer it came with.
        transfer: TransferId,
        /// The sender, after S2.
        from: String,
        /// The text, after S2.
        text: String,
    },
    /// The code a wormhole send is waiting on (F-MW1).
    WormholeCode {
        /// The transfer.
        transfer: TransferId,
        /// The code to read out.
        code: String,
        /// The same code as a QR code.
        qr: QrCode,
    },
    /// Paired devices that accept Object Push.
    BluetoothDevices {
        /// The devices.
        devices: Vec<BluetoothDevice>,
    },
}

/// A machine-readable error with a short English message for logs. The UI
/// translates `code`, not `message`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorInfo {
    /// What went wrong.
    pub code: ErrorCode,
    /// Detail, never containing peer-supplied text or file names.
    pub message: String,
}

/// What went wrong, for the UI to translate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The command was malformed.
    BadCommand,
    /// The command named an API version this engine does not speak.
    BadVersion,
    /// The protocol is not in this build, or disabled in the settings.
    Unavailable,
    /// The peer, offer or transfer named does not exist (any more).
    NotFound,
    /// A settings value was refused.
    BadSettings,
    /// A file to send is missing, not a regular file, or unreadable.
    BadFile,
    /// Over a limit: size, count, text length, concurrent transfers.
    TooLarge,
    /// The peer refused, or the user declined.
    Refused,
    /// The peer's certificate did not match what it announced (F-LS3).
    PeerMismatch,
    /// A wormhole code that is malformed, or wrong.
    BadCode,
    /// A network failure or timeout.
    Network,
    /// The file system said no, or there is no space.
    Storage,
    /// Anything else.
    Internal,
}

/// How one protocol is doing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolStatus {
    /// The protocol.
    pub protocol: Protocol,
    /// Its state.
    pub state: ProtocolState,
    /// Why it failed, for the settings page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
}

/// A receiver's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolState {
    /// Off, by the switch or the settings.
    Off,
    /// Coming up.
    Starting,
    /// Listening.
    Ready,
    /// Could not start; see `error`.
    Failed,
    /// Nothing to receive: Bluetooth receiving is the system's (F-BT2), and
    /// wormhole receives only by code.
    SendOnly,
}

/// A peer that can be sent to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    /// Opaque, stable while the peer is visible.
    pub id: String,
    /// Which protocol found it.
    pub protocol: Protocol,
    /// Its name, after S2.
    pub name: String,
    /// Its model, after S2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// What kind of device it says it is.
    pub device_type: DeviceType,
}

/// What a peer says it is. Only used to pick an icon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceType {
    /// A phone.
    Phone,
    /// A tablet.
    Tablet,
    /// A desktop or laptop.
    Computer,
    /// Anything else.
    #[default]
    Unknown,
}

/// An offer as the consent dialog shows it (F-C2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferView {
    /// Answer with this.
    pub id: OfferId,
    /// Which protocol.
    pub protocol: Protocol,
    /// The sender, after S2.
    pub sender: String,
    /// The sender's model, after S2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Up to [`MAX_LISTED_FILES`] files.
    pub files: Vec<FileView>,
    /// Files not listed.
    pub more_files: usize,
    /// How many files in all.
    pub file_count: usize,
    /// Their total size.
    pub total_bytes: u64,
    /// Whether the offer carries a text.
    pub has_text: bool,
    /// The PIN to compare with the sender's screen (F-QS3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    /// Seconds until the offer is declined on its own (F-C3).
    pub expires_in: u64,
}

/// One file, as the UI shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileView {
    /// The name, after S1.
    pub name: String,
    /// The size.
    pub size: u64,
}

/// Which way a transfer goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// To this phone.
    Incoming,
    /// From this phone.
    Outgoing,
}

/// A transfer as the UI lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferView {
    /// Cancel with this.
    pub id: TransferId,
    /// Which way.
    pub direction: Direction,
    /// Which protocol.
    pub protocol: Protocol,
    /// The other side, after S2.
    pub peer: String,
    /// Up to [`MAX_LISTED_FILES`] files.
    pub files: Vec<FileView>,
    /// How many files in all.
    pub file_count: usize,
    /// Bytes in all.
    pub total_bytes: u64,
}

/// How a transfer ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Outcome {
    /// Everything arrived or was delivered.
    Done,
    /// The user or the peer cancelled.
    Cancelled,
    /// It failed.
    Failed {
        /// Why.
        error: ErrorInfo,
    },
}

/// A QR code, as rows of `0` and `1`, dark modules `1`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QrCode {
    /// Modules per side.
    pub size: u32,
    /// `size` strings of `size` characters.
    pub rows: Vec<String>,
}

/// A paired Bluetooth device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BluetoothDevice {
    /// `AA:BB:CC:DD:EE:FF`.
    pub address: String,
    /// Its name, after S2.
    pub name: String,
}

/// Why a command could not be parsed.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    /// Over [`MAX_MESSAGE_BYTES`].
    #[error("command is larger than {MAX_MESSAGE_BYTES} bytes")]
    TooLarge,
    /// Not a command.
    #[error("malformed command: {0}")]
    Malformed(String),
    /// Another API version.
    #[error("unsupported API version {0}")]
    Version(u32),
}

impl ErrorInfo {
    /// An error with a message. The message goes through S2 and is capped
    /// at [`MAX_ERROR_MESSAGE_CHARS`]: it ends up in logs, and a message
    /// that quotes its input (serde's do) must not carry control characters
    /// or unbounded length there.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        // CONTRACT: same signature; the message is now sanitised and capped.
        let message: String = message.into();
        ErrorInfo {
            code,
            message: text::display(&message, MAX_ERROR_MESSAGE_CHARS),
        }
    }
}

impl From<ParseError> for ErrorInfo {
    fn from(e: ParseError) -> Self {
        let code = match e {
            ParseError::Version(_) => ErrorCode::BadVersion,
            ParseError::TooLarge | ParseError::Malformed(_) => ErrorCode::BadCommand,
        };
        ErrorInfo::new(code, e.to_string())
    }
}

/// Parses one command, holding it to the size cap and the version.
///
/// The `id` is recovered on a best-effort basis when the rest is malformed,
/// so the UI still gets a [`Event::Reply`] it can match.
///
/// The work is linear in the input and the input is at most
/// [`MAX_MESSAGE_BYTES`]: the length is checked before a byte is parsed,
/// serde_json's recursion limit (128) bounds nesting, and a malformed
/// command costs at most two passes (the second only looks for `id`). The
/// fuzz target in `fuzz/` drives this function.
///
/// # Errors
///
/// [`ParseError`], with the id if one could be read.
pub fn parse_command(json: &str) -> Result<CommandEnvelope, (Option<RequestId>, ParseError)> {
    if json.len() > MAX_MESSAGE_BYTES {
        return Err((None, ParseError::TooLarge));
    }
    if !is_json_object(json) {
        return Err((
            None,
            ParseError::Malformed("a command is a JSON object".to_owned()),
        ));
    }
    match serde_json::from_str::<CommandEnvelope>(json) {
        Ok(env) if env.v != API_VERSION => Err((Some(env.id), ParseError::Version(env.v))),
        Ok(env) => Ok(env),
        Err(e) => {
            #[derive(Deserialize)]
            struct IdOnly {
                id: RequestId,
            }
            let id = serde_json::from_str::<IdOnly>(json).ok().map(|i| i.id);
            Err((id, ParseError::Malformed(short(&e))))
        }
    }
}

/// Parses the start configuration, with the same caps as a command.
///
/// # Errors
///
/// [`ParseError`].
pub fn parse_start_config(json: &str) -> Result<StartConfig, ParseError> {
    if json.len() > MAX_MESSAGE_BYTES {
        return Err(ParseError::TooLarge);
    }
    if !is_json_object(json) {
        return Err(ParseError::Malformed(
            "the start configuration is a JSON object".to_owned(),
        ));
    }
    let cfg: StartConfig =
        serde_json::from_str(json).map_err(|e| ParseError::Malformed(short(&e)))?;
    if cfg.v != API_VERSION {
        return Err(ParseError::Version(cfg.v));
    }
    Ok(cfg)
}

/// The variant without fields that the tag named: no other key may be
/// there. The keys left once serde has taken the tag are read as a struct
/// with no fields that refuses unknown ones, which is what a struct variant
/// with fields gets.
fn no_fields<'de, D: serde::Deserializer<'de>>(d: D) -> Result<(), D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct NoFields {}
    NoFields::deserialize(d).map(|NoFields {}| ())
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Whether `json` is an object at the top level. serde's derived structs
/// also accept the positional form `[1, 7, {...}]`, where
/// `deny_unknown_fields` means nothing; the envelope is only ever an object.
fn is_json_object(json: &str) -> bool {
    json.trim_start_matches([' ', '\t', '\n', '\r'])
        .starts_with('{')
}

/// A serde error as a log line: S2, and capped. serde quotes unknown
/// variants and fields verbatim, and the command may be 64 KiB of them.
fn short(e: &serde_json::Error) -> String {
    text::display(&e.to_string(), MAX_ERROR_MESSAGE_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse() {
        let env =
            parse_command(r#"{"v":1,"id":7,"cmd":{"type":"set_receiving","on":true}}"#).unwrap();
        assert_eq!(env.id, 7);
        assert_eq!(env.cmd, Command::SetReceiving { on: true });
        let env = parse_command(r#"{"v":1,"id":8,"cmd":{"type":"get_settings"}}"#).unwrap();
        assert_eq!(env.cmd, Command::GetSettings);
        let env = parse_command(
            r#"{"v":1,"id":9,"cmd":{"type":"send","target":{"protocol":"local_send","peer":"p1"},"items":[{"kind":"file","path":"/a"},{"kind":"text","text":"hi"}]}}"#,
        )
        .unwrap();
        assert!(matches!(env.cmd, Command::Send { .. }));
    }

    #[test]
    fn unknown_fields_and_versions_are_refused() {
        assert!(matches!(
            parse_command(r#"{"v":1,"id":7,"cmd":{"type":"set_receiving","on":true,"x":1}}"#),
            Err((Some(7), ParseError::Malformed(_)))
        ));
        assert!(matches!(
            parse_command(r#"{"v":1,"id":7,"x":0,"cmd":{"type":"get_settings"}}"#),
            Err((Some(7), ParseError::Malformed(_)))
        ));
        assert!(matches!(
            parse_command(r#"{"v":2,"id":7,"cmd":{"type":"get_settings"}}"#),
            Err((Some(7), ParseError::Version(2)))
        ));
        assert!(matches!(
            parse_command(r#"{"v":1,"id":7,"cmd":{"type":"self_destruct"}}"#),
            Err((Some(7), ParseError::Malformed(_)))
        ));
        assert!(matches!(
            parse_command("nonsense"),
            Err((None, ParseError::Malformed(_)))
        ));
        let big = format!(
            r#"{{"v":1,"id":1,"cmd":{{"type":"receive_wormhole","code":"{}"}}}}"#,
            "a".repeat(MAX_MESSAGE_BYTES)
        );
        assert!(matches!(
            parse_command(&big),
            Err((None, ParseError::TooLarge))
        ));
    }

    /// Every command and target without fields refuses a field, as those
    /// with fields do; the tag alone, and nothing else, is the command.
    #[test]
    fn variants_without_fields_refuse_fields() {
        for kind in [
            "get_settings",
            "start_discovery",
            "stop_discovery",
            "list_bluetooth_devices",
        ] {
            let bare = format!(r#"{{"v":1,"id":3,"cmd":{{"type":"{kind}"}}}}"#);
            assert!(parse_command(&bare).is_ok(), "{kind}");
            for extra in [
                r#""x":1"#,
                r#""auto_accept":true"#,
                r#""type":"get_settings""#,
            ] {
                let json = format!(r#"{{"v":1,"id":3,"cmd":{{"type":"{kind}",{extra}}}}}"#);
                assert!(
                    matches!(
                        parse_command(&json),
                        Err((Some(3), ParseError::Malformed(_)))
                    ),
                    "{json}"
                );
            }
        }
        let send = |target: &str| {
            format!(
                r#"{{"v":1,"id":4,"cmd":{{"type":"send","target":{target},"items":[{{"kind":"text","text":"hi"}}]}}}}"#
            )
        };
        let env = parse_command(&send(r#"{"protocol":"wormhole"}"#)).unwrap();
        assert!(matches!(
            env.cmd,
            Command::Send {
                target: SendTarget::Wormhole,
                ..
            }
        ));
        for target in [
            r#"{"protocol":"wormhole","peer":"x"}"#,
            r#"{"protocol":"wormhole","code":"7-foo"}"#,
            r#"{"peer":"x","protocol":"wormhole"}"#,
        ] {
            assert!(
                matches!(
                    parse_command(&send(target)),
                    Err((Some(4), ParseError::Malformed(_)))
                ),
                "{target}"
            );
        }
        // What they serialise to is unchanged, and reads back.
        for cmd in [
            Command::GetSettings,
            Command::StartDiscovery,
            Command::StopDiscovery,
            Command::ListBluetoothDevices,
        ] {
            let json = serde_json::to_string(&cmd).unwrap();
            assert!(!json.contains(','), "{json}");
            assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
        }
        assert_eq!(
            serde_json::to_string(&SendTarget::Wormhole).unwrap(),
            r#"{"protocol":"wormhole"}"#
        );
    }

    #[test]
    fn a_settings_event_says_recovered_only_when_it_was() {
        let e = |recovered| Event::Settings {
            settings: Settings::default(),
            effective_device_name: "P".into(),
            recovered,
        };
        let json = serde_json::to_string(&e(false)).unwrap();
        assert!(!json.contains("recovered"), "{json}");
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), e(false));
        let json = serde_json::to_string(&e(true)).unwrap();
        assert!(json.ends_with(r#","recovered":true}"#), "{json}");
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), e(true));
    }

    #[test]
    fn events_serialise_with_their_tag() {
        let e = Event::Reply {
            id: 1,
            ok: true,
            error: None,
            transfer: None,
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"reply","id":1,"ok":true}"#
        );
        let e = Event::TransferFinished {
            transfer: 3,
            outcome: Outcome::Failed {
                error: ErrorInfo::new(ErrorCode::Network, "timeout"),
            },
            saved: vec![],
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"transfer_finished","transfer":3,"outcome":{"result":"failed","error":{"code":"network","message":"timeout"}}}"#
        );
    }

    #[test]
    fn the_positional_form_is_refused() {
        assert!(matches!(
            parse_command(r#"[1, 5, {"type":"get_settings"}]"#),
            Err((None, ParseError::Malformed(_)))
        ));
        assert!(parse_command(" \n{\"v\":1,\"id\":1,\"cmd\":{\"type\":\"get_settings\"}}").is_ok());
        assert!(parse_start_config(r#"[1, "/d", "/dl"]"#).is_err());
    }

    #[test]
    fn start_config_parses_strictly() {
        let c = parse_start_config(r#"{"v":1,"data_dir":"/d","download_dir":"/dl"}"#).unwrap();
        assert!(!c.allow_loopback);
        assert!(
            parse_start_config(r#"{"v":1,"data_dir":"/d","download_dir":"/dl","extra":1}"#)
                .is_err()
        );
    }

    /// A configuration the parser accepts survives its own serialiser, even
    /// at the cap and without the optional fields, which the serialiser once
    /// wrote out as `null` and `false` and pushed it over.
    #[test]
    fn start_config_at_the_cap_round_trips() {
        let frame = r#"{"v":1,"data_dir":"","download_dir":"/dl"}"#;
        let json = format!(
            r#"{{"v":1,"data_dir":"{}","download_dir":"/dl"}}"#,
            "d".repeat(MAX_MESSAGE_BYTES - frame.len())
        );
        assert_eq!(json.len(), MAX_MESSAGE_BYTES);
        let cfg = parse_start_config(&json).unwrap();
        let again = serde_json::to_string(&cfg).unwrap();
        assert!(
            again.len() <= json.len(),
            "{} > {}",
            again.len(),
            json.len()
        );
        assert_eq!(parse_start_config(&again).unwrap(), cfg);

        // The optional fields still go out when they are set.
        let set = StartConfig {
            data_dir: "/d".to_owned(),
            device_model: Some("Xperia 10 III".to_owned()),
            allow_loopback: true,
            ..cfg
        };
        let again = serde_json::to_string(&set).unwrap();
        assert!(again.contains(r#""device_model":"Xperia 10 III""#));
        assert!(again.contains(r#""allow_loopback":true"#));
        assert_eq!(parse_start_config(&again).unwrap(), set);
    }
}
