//! One Object Push send, through obexd on the session bus (F-BT1).
//!
//! ```text
//! Client1.CreateSession(address, {Target: "opp"})    -> session
//! AddMatch(PropertiesChanged under session, from that obexd)
//! for each file:
//!     ObjectPush1.SendFile(path)                      -> transfer, {Size, ...}
//!     PropertiesChanged(Transfer1: Status, Transferred) ... "complete" | "error"
//!     (Transfer1.Cancel() on user cancel, idle timeout, or anything odd)
//! Client1.RemoveSession(session)                      always
//! close the connection                                always
//! ```
//!
//! **Cleanup is layered.** `Cancel` and `RemoveSession` are always tried,
//! each within [`Timeouts::cleanup`]. Whatever they miss, the connection
//! close catches: obexd watches the owner of every session it creates and
//! tears the session (and its transfers) down when that owner leaves the
//! bus. Each send has a private connection of its own for exactly this
//! reason, so a `CreateSession` that is abandoned before it answers still
//! leaves nothing behind.
//!
//! **Everything obexd says is checked.** Replies must come from the name
//! that answered `CreateSession`; the transfer must live under the
//! session's path; `Status` must be one of obexd's five words;
//! `Transferred` and `Size` must not exceed the size the hub checked; a
//! transfer whose object disappears without a final status fails.
//!
//! **What obexd reads is not what the hub checked (TOCTOU).** obexd opens
//! the file by path itself, some time after the hub's `stat`, and outside
//! Sukkula's sandbox. Between the two the path could be replaced -- by a
//! different file, or a symlink to any file the user can read -- and obexd
//! would send that. Only a process running as the same user can do this,
//! and such a process can already read every file obexd could; the threat
//! model here is peers in radio range, who cannot touch the path at all.
//! What this module does narrow: it re-checks right before `SendFile` that
//! the path is still a regular file of the checked size, refuses a
//! transfer whose `Size` differs, and cancels one whose `Transferred`
//! grows past it. A file that is *rewritten* in place with the same size
//! is sent as it is when obexd reads it.
//!
//! The name the receiver sees is the file's own base name, as obexd takes
//! it from the path; `OutgoingFile::name` cannot be passed without writing
//! a renamed copy, and only sukkula-core writes files (S3). It is our own
//! user's file name, sent where the user chose to send it.

use std::collections::VecDeque;
use std::path::Path as FsPath;
use std::time::{Duration, Instant};

use dbus::Message;
use dbus::arg::{ArgType, Iter, Variant};
use dbus::strings::Path;
use tokio_util::sync::CancellationToken;

use super::bluez::{Address, for_each_property};
use super::bus::{self, Bus, BusError, PROPERTIES_IFACE, TICK};
use crate::adapter::OutgoingFile;
use crate::api::{ErrorCode, ErrorInfo};
use crate::ctx::{TransferHandle, cancelled};

const OBEX_NAME: &str = "org.bluez.obex";
const OBEX_PATH: &str = "/org/bluez/obex";
const CLIENT_IFACE: &str = "org.bluez.obex.Client1";
const PUSH_IFACE: &str = "org.bluez.obex.ObjectPush1";
const TRANSFER_IFACE: &str = "org.bluez.obex.Transfer1";

/// Largest OBEX packet (its length field is 16 bits). The first packet of
/// a push can carry up to this much of the file before the receiver has
/// said yes; see [`Timeouts::consent`].
const MAX_OBEX_PACKET: u64 = 65_535;

/// Most updates kept between two turns of the loop. obexd sends a few per
/// second; the queue is drained every tick, so this only bites a flood,
/// and what a flood drowns out the next poll recovers.
const MAX_UPDATES: usize = 64;

/// How long each wait may take. The defaults are the phone's; tests
/// shorten them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// Any single local call: `Hello`, `AddMatch`, `SendFile`,
    /// `GetManagedObjects`, a property poll.
    pub call: Duration,
    /// `CreateSession`, which pages the device and opens RFCOMM and OBEX
    /// before it answers.
    pub connect: Duration,
    /// No progress for this long, while the receiver may still be asking
    /// its user: until more than one OBEX packet of the file has gone.
    pub consent: Duration,
    /// No progress for this long, once data is flowing (S6).
    pub idle: Duration,
    /// How often the transfer's properties are read directly, in case a
    /// signal was lost or the object vanished.
    pub poll: Duration,
    /// Each of `Cancel` and `RemoveSession` on the way out. Both together
    /// stay well inside the engine's 3 s stop budget.
    pub cleanup: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        use sukkula_core::limits::{HANDSHAKE_TIMEOUT, NETWORK_IDLE_TIMEOUT, OFFER_TIMEOUT};
        Timeouts {
            call: Duration::from_secs(5),
            connect: HANDSHAKE_TIMEOUT,
            // The receiving phone shows its user a dialog, as we do (F-C2);
            // give them the time we give ours (F-C3), not the 30 s a stalled
            // network read gets.
            consent: OFFER_TIMEOUT,
            idle: NETWORK_IDLE_TIMEOUT,
            poll: Duration::from_secs(2),
            cleanup: Duration::from_millis(750),
        }
    }
}

/// Sends `files` to `address`, in order, reporting progress on `handle`.
/// Returns once everything was delivered, or with why not; the session is
/// gone either way.
pub(super) fn send(
    session_bus: &str,
    address: Address,
    files: &[OutgoingFile],
    timeouts: &Timeouts,
    handle: &TransferHandle,
) -> Result<(), ErrorInfo> {
    let cancel = handle.token();
    let mut bus = Bus::connect(session_bus, timeouts.call, Some(cancel))
        .map_err(|e| service_error(&e, cancel))?;
    let session = create_session(&mut bus, address, timeouts, cancel)?;
    let result = push_all(&mut bus, &session, files, timeouts, handle);
    remove_session(&mut bus, &session, timeouts);
    result
}

/// A session obexd made for us.
struct Session {
    path: Path<'static>,
    /// The unique name of the obexd that made it: the only sender whose
    /// replies and signals count.
    owner: String,
}

fn create_session(
    bus: &mut Bus,
    address: Address,
    timeouts: &Timeouts,
    cancel: &CancellationToken,
) -> Result<Session, ErrorInfo> {
    let destination = address.to_string();
    let mut args = std::collections::HashMap::new();
    args.insert("Target", Variant("opp"));
    let call = bus::method_call(OBEX_NAME, OBEX_PATH, CLIENT_IFACE, "CreateSession")
        .map_err(|e| service_error(&e, cancel))?
        .append2(destination.as_str(), args);
    let reply = bus
        .call_plain(call, timeouts.connect, Some(cancel))
        .map_err(|e| match e {
            BusError::Remote(ref name) if name.starts_with("org.bluez.obex.Error.") => {
                tracing::debug!(error = %e, "CreateSession refused");
                ErrorInfo::new(ErrorCode::Network, "the device could not be reached")
            }
            BusError::Timeout => {
                ErrorInfo::new(ErrorCode::Network, "the device did not answer in time")
            }
            _ => service_error(&e, cancel),
        })?;
    let owner = reply
        .sender()
        .map(|s| s.to_string())
        .filter(|s| s.starts_with(':'))
        .ok_or_else(malformed)?;
    let path = reply
        .get1::<Path<'_>>()
        .map(Path::into_static)
        .filter(|p| &**p != "/")
        .ok_or_else(malformed)?;
    let session = Session { path, owner };
    // Subscribe before the first SendFile, so no status of any transfer in
    // this session can pass unseen. Only this obexd's signals, only under
    // this session's path.
    let rule = format!(
        "type='signal',sender='{}',interface='{PROPERTIES_IFACE}',member='PropertiesChanged',path_namespace='{}',arg0='{TRANSFER_IFACE}'",
        session.owner, &*session.path
    );
    let owner_rule = format!(
        "type='signal',sender='{}',path='{}',interface='{}',member='NameOwnerChanged',arg0='{OBEX_NAME}'",
        bus::DBUS_NAME,
        bus::DBUS_PATH,
        bus::DBUS_IFACE
    );
    let subscribed = bus
        .add_match(&rule, timeouts.call, Some(cancel))
        .and_then(|()| bus.add_match(&owner_rule, timeouts.call, Some(cancel)));
    if let Err(e) = subscribed {
        remove_session(bus, &session, timeouts);
        return Err(service_error(&e, cancel));
    }
    Ok(session)
}

fn remove_session(bus: &mut Bus, session: &Session, timeouts: &Timeouts) {
    let call = bus::method_call(OBEX_NAME, OBEX_PATH, CLIENT_IFACE, "RemoveSession")
        .map(|m| m.append1(session.path.clone()));
    // No cancel token: this must run after a cancel too. Bounded by its
    // own timeout, and backed by the connection close that follows.
    match call.and_then(|c| bus.call_plain(c, timeouts.cleanup, None)) {
        Ok(_) => {}
        Err(e) => tracing::debug!(error = %e, "RemoveSession failed; the disconnect will end it"),
    }
}

fn push_all(
    bus: &mut Bus,
    session: &Session,
    files: &[OutgoingFile],
    timeouts: &Timeouts,
    handle: &TransferHandle,
) -> Result<(), ErrorInfo> {
    for file in files {
        push_one(bus, session, file, timeouts, handle)?;
    }
    Ok(())
}

fn push_one(
    bus: &mut Bus,
    session: &Session,
    file: &OutgoingFile,
    timeouts: &Timeouts,
    handle: &TransferHandle,
) -> Result<(), ErrorInfo> {
    let cancel = handle.token();
    let path = source_path(file)?;
    let mut watch = Watch::new(session);
    let call = bus::method_call(OBEX_NAME, &session.path, PUSH_IFACE, "SendFile")
        .map_err(|e| service_error(&e, cancel))?
        .append1(path);
    let reply = bus
        .call(call, timeouts.call, Some(cancel), &mut |m| {
            watch.on_signal(m)
        })
        .map_err(|e| match e {
            // obexd could not open or stat the file.
            BusError::Remote(ref name) if name == "org.bluez.obex.Error.InvalidArguments" => {
                ErrorInfo::new(ErrorCode::BadFile, "the file cannot be read")
            }
            BusError::Remote(ref name) if name.starts_with("org.bluez.obex.Error.") => {
                tracing::debug!(error = %e, "SendFile refused");
                ErrorInfo::new(ErrorCode::Network, "the device refused the file")
            }
            _ => service_error(&e, cancel),
        })?;
    if reply.sender().is_none_or(|s| *s != *session.owner) {
        return Err(malformed());
    }
    let mut it = reply.iter_init();
    let transfer = it
        .get::<Path<'_>>()
        .map(Path::into_static)
        .filter(|t| under(t, &session.path))
        .ok_or_else(malformed)?;
    it.next();
    let initial = it
        .recurse(ArgType::Array)
        .map(read_update)
        .unwrap_or_default();

    let mut state = FileState::new(file.size);
    let outcome = match state.apply(&initial, handle) {
        Some(outcome) => outcome,
        None => {
            watch.transfer = Some(transfer.clone());
            wait(bus, &mut watch, &transfer, &mut state, timeouts, handle)
        }
    };
    match outcome {
        Ended::Complete => Ok(()),
        Ended::Failed(e) => Err(e),
        Ended::Abort(e) => {
            cancel_transfer(bus, &transfer, timeouts);
            Err(e)
        }
    }
}

/// The path as obexd must get it: absolute, UTF-8 (a D-Bus string), no
/// NUL, and still the regular file of the size the hub checked.
fn source_path(file: &OutgoingFile) -> Result<&str, ErrorInfo> {
    let bad = |m: &'static str| ErrorInfo::new(ErrorCode::BadFile, m);
    let path = file
        .path
        .to_str()
        .filter(|p| !p.contains('\0') && FsPath::new(p).is_absolute())
        .ok_or_else(|| bad("the file's path cannot be handed to the Bluetooth service"))?;
    // Narrows the window described in the module docs; it cannot close it.
    let meta = std::fs::metadata(path).map_err(|_| bad("the file cannot be read"))?;
    if !meta.is_file() || meta.len() != file.size {
        return Err(bad("the file changed after it was chosen"));
    }
    Ok(path)
}

fn under(child: &str, parent: &str) -> bool {
    child
        .strip_prefix(parent)
        .is_some_and(|rest| rest.len() > 1 && rest.starts_with('/'))
}

fn cancel_transfer(bus: &mut Bus, transfer: &Path<'static>, timeouts: &Timeouts) {
    let call = bus::method_call(OBEX_NAME, transfer, TRANSFER_IFACE, "Cancel");
    match call.and_then(|c| bus.call_plain(c, timeouts.cleanup, None)) {
        Ok(_) => {}
        Err(e) => tracing::debug!(error = %e, "Transfer1.Cancel failed; RemoveSession follows"),
    }
}

/// How one file's transfer ended.
#[derive(Debug)]
enum Ended {
    /// obexd said "complete".
    Complete,
    /// obexd said "error", or the transfer is gone: nothing to cancel.
    Failed(ErrorInfo),
    /// We are stopping it: cancel it first.
    Abort(ErrorInfo),
}

/// obexd's transfer states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Queued,
    Active,
    Suspended,
    Complete,
    Error,
}

impl Status {
    fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "queued" => Status::Queued,
            "active" => Status::Active,
            "suspended" => Status::Suspended,
            "complete" => Status::Complete,
            "error" => Status::Error,
            _ => return None,
        })
    }
}

/// The part of a Transfer1 property set we act on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Update {
    status: Option<Status>,
    transferred: Option<u64>,
    size: Option<u64>,
}

/// Reads `Status`, `Transferred` and `Size` from an `a{sv}`. Wrong types
/// and unknown statuses count as absent.
fn read_update(props: Iter<'_>) -> Update {
    let mut u = Update::default();
    for_each_property(props, |key, mut value| match key {
        "Status" if u.status.is_none() => u.status = value.get::<&str>().and_then(Status::parse),
        "Transferred" if u.transferred.is_none() => u.transferred = value.get::<u64>(),
        "Size" if u.size.is_none() => u.size = value.get::<u64>(),
        _ => {}
    });
    u
}

/// Signals for this send, parsed on arrival so no message is held.
struct Watch {
    owner: String,
    session: Path<'static>,
    transfer: Option<Path<'static>>,
    updates: VecDeque<(Path<'static>, Update)>,
    owner_lost: bool,
}

impl Watch {
    fn new(session: &Session) -> Watch {
        Watch {
            owner: session.owner.clone(),
            session: session.path.clone(),
            transfer: None,
            updates: VecDeque::new(),
            owner_lost: false,
        }
    }

    fn on_signal(&mut self, m: &Message) {
        let Some(sender) = m.sender() else {
            return;
        };
        let member = m.member();
        let member = member.as_deref().unwrap_or("");
        if *sender == *bus::DBUS_NAME && member == "NameOwnerChanged" {
            let (name, _old, new) = m.get3::<&str, &str, &str>();
            if name == Some(OBEX_NAME) && new != Some(self.owner.as_str()) {
                self.owner_lost = true;
            }
            return;
        }
        if *sender != *self.owner
            || member != "PropertiesChanged"
            || m.interface().is_none_or(|i| &*i != PROPERTIES_IFACE)
        {
            return;
        }
        let Some(path) = m.path() else {
            return;
        };
        // Before SendFile's reply names the transfer, anything under the
        // session may be it; after, only it.
        let relevant = match &self.transfer {
            Some(t) => *path == **t,
            None => under(&path, &self.session),
        };
        if !relevant {
            return;
        }
        let mut it = m.iter_init();
        if it.get::<&str>() != Some(TRANSFER_IFACE) {
            return;
        }
        it.next();
        let Some(changed) = it.recurse(ArgType::Array) else {
            return;
        };
        let update = read_update(changed);
        if self.updates.len() < MAX_UPDATES {
            self.updates.push_back((path.into_static(), update));
        }
    }
}

/// Progress of one file.
struct FileState {
    size: u64,
    sent: u64,
    status: Option<Status>,
    last_activity: Instant,
}

impl FileState {
    fn new(size: u64) -> FileState {
        FileState {
            size,
            sent: 0,
            status: None,
            last_activity: Instant::now(),
        }
    }

    /// Applies one update. `Some` when the transfer is over.
    fn apply(&mut self, u: &Update, handle: &TransferHandle) -> Option<Ended> {
        if u.size.is_some_and(|s| s != self.size) {
            return Some(Ended::Abort(changed()));
        }
        if let Some(t) = u.transferred {
            if t > self.size {
                return Some(Ended::Abort(changed()));
            }
            // Never backwards: a smaller value is ignored, not subtracted.
            if t > self.sent {
                handle.add_progress(t.saturating_sub(self.sent));
                self.sent = t;
                self.last_activity = Instant::now();
            }
        }
        if let Some(s) = u.status
            && self.status != Some(s)
        {
            self.status = Some(s);
            self.last_activity = Instant::now();
            match s {
                Status::Complete => {
                    // "complete" means every byte went; show it.
                    handle.add_progress(self.size.saturating_sub(self.sent));
                    self.sent = self.size;
                    return Some(Ended::Complete);
                }
                Status::Error => return Some(Ended::Failed(transfer_error(self.sent))),
                Status::Queued | Status::Active | Status::Suspended => {}
            }
        }
        None
    }

    /// How long no progress is tolerated now.
    fn allowance(&self, timeouts: &Timeouts) -> Duration {
        if self.sent <= MAX_OBEX_PACKET {
            timeouts.consent.max(timeouts.idle)
        } else {
            timeouts.idle
        }
    }
}

/// Waits for the transfer to end: by status, by vanishing, by cancel, by
/// the bus or obexd going away, or by the idle timeout.
fn wait(
    bus: &mut Bus,
    watch: &mut Watch,
    transfer: &Path<'static>,
    state: &mut FileState,
    timeouts: &Timeouts,
    handle: &TransferHandle,
) -> Ended {
    let cancel = handle.token();
    let mut next_poll = Instant::now().checked_add(timeouts.poll);
    let mut vanished = false;
    loop {
        while let Some((path, update)) = watch.updates.pop_front() {
            if path == *transfer
                && let Some(ended) = state.apply(&update, handle)
            {
                return ended;
            }
        }
        if vanished {
            return Ended::Failed(ErrorInfo::new(
                ErrorCode::Network,
                "the transfer ended without a result",
            ));
        }
        if watch.owner_lost {
            return Ended::Failed(ErrorInfo::new(
                ErrorCode::Unavailable,
                "the Bluetooth file service stopped",
            ));
        }
        if cancel.is_cancelled() {
            return Ended::Abort(cancelled());
        }
        let now = Instant::now();
        if now.saturating_duration_since(state.last_activity) >= state.allowance(timeouts) {
            return Ended::Abort(ErrorInfo::new(
                ErrorCode::Network,
                "the device stopped responding",
            ));
        }
        if next_poll.is_some_and(|t| now >= t) {
            next_poll = now.checked_add(timeouts.poll);
            match poll(bus, watch, transfer, timeouts, cancel) {
                Ok(update) => {
                    // Signals that came in during the poll are older than
                    // its answer; they go first.
                    let path = transfer.clone();
                    if watch.updates.len() < MAX_UPDATES {
                        watch.updates.push_back((path, update));
                    }
                }
                // Anything obexd sent before the error reply is already
                // queued, in order; once that is applied, a transfer with
                // no final status has failed.
                Err(e) if e.is_unknown_object() => vanished = true,
                Err(BusError::Cancelled) => return Ended::Abort(cancelled()),
                Err(BusError::Disconnected) => {
                    return Ended::Failed(ErrorInfo::new(
                        ErrorCode::Unavailable,
                        "the Bluetooth file service went away",
                    ));
                }
                Err(e) => tracing::debug!(error = %e, "transfer poll failed"),
            }
            continue;
        }
        if let Err(e) = bus.pump(TICK, &mut |m| watch.on_signal(m)) {
            tracing::debug!(error = %e, "session bus lost");
            return Ended::Failed(ErrorInfo::new(
                ErrorCode::Unavailable,
                "the Bluetooth file service went away",
            ));
        }
    }
}

fn poll(
    bus: &mut Bus,
    watch: &mut Watch,
    transfer: &Path<'static>,
    timeouts: &Timeouts,
    cancel: &CancellationToken,
) -> Result<Update, BusError> {
    let call =
        bus::method_call(OBEX_NAME, transfer, PROPERTIES_IFACE, "GetAll")?.append1(TRANSFER_IFACE);
    let reply = bus.call(
        call,
        timeouts.call.min(timeouts.poll),
        Some(cancel),
        &mut |m| watch.on_signal(m),
    )?;
    if reply.sender().is_none_or(|s| *s != *watch.owner) {
        return Err(BusError::Malformed);
    }
    let mut it = reply.iter_init();
    it.recurse(ArgType::Array)
        .map(read_update)
        .ok_or(BusError::Malformed)
}

fn changed() -> ErrorInfo {
    ErrorInfo::new(ErrorCode::BadFile, "the file changed after it was chosen")
}

fn malformed() -> ErrorInfo {
    ErrorInfo::new(
        ErrorCode::Unavailable,
        "the Bluetooth file service gave an unexpected reply",
    )
}

/// obexd reports every failure as the same "error". Before a second
/// packet of data has gone, the receiver has not accepted anything yet,
/// and a refusal (its user said no) is by far the likeliest cause; later,
/// the link is.
fn transfer_error(sent: u64) -> ErrorInfo {
    if sent <= MAX_OBEX_PACKET {
        ErrorInfo::new(ErrorCode::Refused, "the device did not accept the file")
    } else {
        ErrorInfo::new(ErrorCode::Network, "the transfer was interrupted")
    }
}

/// Maps a failure to reach or use obexd.
fn service_error(e: &BusError, cancel: &CancellationToken) -> ErrorInfo {
    tracing::debug!(error = %e, "obexd call failed");
    if *e == BusError::Cancelled || cancel.is_cancelled() {
        return cancelled();
    }
    if e.is_absent() {
        return ErrorInfo::new(
            ErrorCode::Unavailable,
            "the Bluetooth file service is not available",
        );
    }
    match e {
        BusError::Timeout => ErrorInfo::new(
            ErrorCode::Unavailable,
            "the Bluetooth file service did not answer",
        ),
        BusError::Remote(name) if name.starts_with("org.freedesktop.DBus.Error.") => {
            // NoReply, Timeout, LimitsExceeded, ...: the bus gave up on it.
            ErrorInfo::new(
                ErrorCode::Unavailable,
                "the Bluetooth file service did not answer",
            )
        }
        BusError::Remote(_) => ErrorInfo::new(
            ErrorCode::Unavailable,
            "the Bluetooth file service refused the request",
        ),
        BusError::Unreachable
        | BusError::Disconnected
        | BusError::Cancelled
        | BusError::Malformed => malformed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfers_must_live_under_the_session() {
        let s = "/org/bluez/obex/client/session1";
        assert!(under("/org/bluez/obex/client/session1/transfer1", s));
        assert!(!under("/org/bluez/obex/client/session1", s));
        assert!(!under("/org/bluez/obex/client/session1/", s));
        assert!(!under("/org/bluez/obex/client/session10/transfer1", s));
        assert!(!under("/org/bluez/obex/client/session2/transfer1", s));
    }

    #[test]
    fn statuses_are_obexds_five_words_only() {
        assert_eq!(Status::parse("complete"), Some(Status::Complete));
        assert_eq!(Status::parse("Complete"), None);
        assert_eq!(Status::parse("complete\0"), None);
        assert_eq!(Status::parse(""), None);
    }

    #[test]
    fn errors_before_any_real_data_read_as_refusals() {
        assert_eq!(transfer_error(0).code, ErrorCode::Refused);
        assert_eq!(transfer_error(MAX_OBEX_PACKET).code, ErrorCode::Refused);
        assert_eq!(transfer_error(MAX_OBEX_PACKET + 1).code, ErrorCode::Network);
    }
}
