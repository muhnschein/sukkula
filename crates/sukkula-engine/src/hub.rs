//! The hub: parses commands, drives the adapters, answers every command
//! with exactly one `Reply`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use sukkula_core::Protocol;
use sukkula_core::config::{MAX_SETTINGS_BYTES, SETTINGS_FILE, Settings};
use sukkula_core::consent::{ConsentBroker, ConsentEvent, Decision};
use sukkula_core::inbox::Inbox;
use sukkula_core::limits::{
    MAX_FILE_BYTES, MAX_FILES_PER_OFFER, MAX_MESSAGE_BYTES, MAX_MODEL_CHARS, MAX_OFFER_BYTES,
    OFFER_TIMEOUT,
};
use sukkula_core::name;
use sukkula_core::reach::ReachPolicy;
use sukkula_core::store::Store;
use sukkula_core::text;
use tokio::sync::Mutex as AsyncMutex;

use crate::adapter::{Adapter, Outgoing, OutgoingFile};
use crate::api::{
    API_VERSION, Command, CommandEnvelope, ErrorCode, ErrorInfo, Event, FileView, MAX_LISTED_FILES,
    OfferView, ProtocolState, ProtocolStatus, SendItem, SendTarget, StartConfig, TransferId,
    parse_command,
};
use crate::ctx::{Ctx, EventSink};

/// How long [`Engine::stop`] waits for adapters and tasks to wind down.
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// Largest `/etc/hw-release` read.
const MAX_HW_RELEASE_BYTES: u64 = 4096;

/// A running engine.
pub struct Engine {
    runtime: Option<tokio::runtime::Runtime>,
    hub: Arc<Hub>,
    gate: Arc<Gate>,
}

struct Hub {
    ctx: Arc<Ctx>,
    adapters: Vec<Arc<dyn Adapter>>,
    state: AsyncMutex<HubState>,
}

#[derive(Default)]
struct HubState {
    receiving: bool,
    statuses: Vec<ProtocolStatus>,
}

/// Stands between the engine and the sink: once closed, nothing reaches the
/// sink, and closing waits for any call already in it.
struct Gate {
    open: AtomicBool,
    sink: RwLock<Option<EventSink>>,
}

impl Gate {
    fn emit(&self, event: Event) {
        if !self.open.load(Ordering::Acquire) {
            return;
        }
        let guard = self.sink.read().unwrap_or_else(PoisonError::into_inner);
        if let Some(sink) = guard.as_ref() {
            sink(event);
        }
    }

    fn close(&self) {
        self.open.store(false, Ordering::Release);
        // Taking the write lock waits out every emit in progress.
        self.sink
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
}

impl Engine {
    /// Starts an engine. Emits `Started`, `Settings` and `Receiving`, in that
    /// order, before any other event.
    ///
    /// # Errors
    ///
    /// The configuration is unusable: a relative path, a directory that
    /// cannot be created, or a runtime that cannot start.
    pub fn start(config: StartConfig, sink: EventSink) -> Result<Engine, ErrorInfo> {
        if config.v != API_VERSION {
            return Err(ErrorInfo::new(
                ErrorCode::BadVersion,
                "unsupported API version",
            ));
        }
        let data_dir = absolute(&config.data_dir)?;
        let download_dir = absolute(&config.download_dir)?;
        let store = Store::open(&data_dir)
            .map_err(|e| ErrorInfo::new(ErrorCode::Storage, e.to_string()))?;
        let inbox = Inbox::open(&download_dir)
            .map_err(|e| ErrorInfo::new(ErrorCode::Storage, e.to_string()))?;
        let settings = load_settings(&store);
        let model = config
            .device_model
            .map_or_else(read_hw_model, |m| text::display(&m, MAX_MODEL_CHARS));

        let gate = Arc::new(Gate {
            open: AtomicBool::new(true),
            sink: RwLock::new(Some(sink)),
        });
        let events: EventSink = {
            let gate = gate.clone();
            Arc::new(move |e| gate.emit(e))
        };
        let consent = ConsentBroker::new(consent_observer(events.clone()));
        let reach = ReachPolicy {
            allow_loopback: config.allow_loopback,
        };
        let ctx = Arc::new(Ctx::new(
            settings, model, store, inbox, consent, reach, events,
        ));

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("sukkula-engine")
            .enable_all()
            .build()
            .map_err(|e| ErrorInfo::new(ErrorCode::Internal, e.to_string()))?;

        let adapters = {
            let _guard = runtime.enter();
            build_adapters(&ctx)
        };
        let hub = Arc::new(Hub {
            ctx: ctx.clone(),
            adapters,
            state: AsyncMutex::new(HubState::default()),
        });

        ctx.emit(Event::Started {
            version: crate::VERSION.to_owned(),
            api: API_VERSION,
            protocols: hub.adapters.iter().map(|a| a.protocol()).collect(),
        });
        hub.emit_settings();
        runtime.block_on(async {
            let mut state = hub.state.lock().await;
            state.statuses = hub.statuses(false);
            hub.emit_receiving(&state);
        });
        Ok(Engine {
            runtime: Some(runtime),
            hub,
            gate,
        })
    }

    /// Takes one command as JSON. Always answered by exactly one `Reply`,
    /// even when the JSON is malformed.
    pub fn command_json(&self, json: &str) {
        match parse_command(json) {
            Ok(env) => self.command(env),
            Err((id, e)) => self.hub.ctx.emit(Event::Reply {
                id: id.unwrap_or(0),
                ok: false,
                error: Some(e.into()),
                transfer: None,
            }),
        }
    }

    /// Takes one parsed command.
    pub fn command(&self, env: CommandEnvelope) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let hub = self.hub.clone();
        runtime.spawn(async move {
            let id = env.id;
            let reply = match hub.dispatch(env.cmd).await {
                Ok(transfer) => Event::Reply {
                    id,
                    ok: true,
                    error: None,
                    transfer,
                },
                Err(error) => Event::Reply {
                    id,
                    ok: false,
                    error: Some(error),
                    transfer: None,
                },
            };
            hub.ctx.emit(reply);
        });
    }

    /// Stops everything. When this returns, no engine thread is running and
    /// the sink will never be called again.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        let hub = self.hub.clone();
        runtime.block_on(async {
            let stopping = async {
                for a in &hub.adapters {
                    a.stop_discovery().await;
                    a.stop_receiving().await;
                }
            };
            let _ = tokio::time::timeout(STOP_TIMEOUT, stopping).await;
        });
        hub.ctx.shut_down();
        runtime.shutdown_timeout(STOP_TIMEOUT);
        self.gate.close();
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Hub {
    async fn dispatch(self: &Arc<Self>, cmd: Command) -> Result<Option<TransferId>, ErrorInfo> {
        match cmd {
            Command::SetReceiving { on } => {
                self.set_receiving(on).await;
                Ok(None)
            }
            Command::SetSettings { settings } => {
                let settings = settings
                    .validate()
                    .map_err(|e| ErrorInfo::new(ErrorCode::BadSettings, e.to_string()))?;
                self.ctx
                    .store()
                    .write_json(SETTINGS_FILE, &settings)
                    .map_err(|e| ErrorInfo::new(ErrorCode::Storage, e.to_string()))?;
                self.ctx.set_settings(settings);
                self.emit_settings();
                let on = self.state.lock().await.receiving;
                if on {
                    self.set_receiving(false).await;
                    self.set_receiving(true).await;
                }
                Ok(None)
            }
            Command::GetSettings => {
                self.emit_settings();
                Ok(None)
            }
            Command::StartDiscovery => {
                for a in self.enabled_adapters() {
                    if let Err(e) = a.start_discovery().await {
                        tracing::debug!(protocol = ?a.protocol(), code = ?e.code, "discovery failed to start");
                    }
                }
                Ok(None)
            }
            Command::StopDiscovery => {
                for a in &self.adapters {
                    a.stop_discovery().await;
                }
                Ok(None)
            }
            Command::Answer { offer, accept } => {
                let decision = if accept {
                    Decision::Accept
                } else {
                    Decision::Decline
                };
                if self.ctx.consent().answer(offer, decision) {
                    Ok(None)
                } else {
                    Err(ErrorInfo::new(ErrorCode::NotFound, "no such offer"))
                }
            }
            Command::Send { target, items } => {
                let protocol = match &target {
                    SendTarget::LocalSend { .. } => Protocol::LocalSend,
                    SendTarget::QuickShare { .. } => Protocol::QuickShare,
                    SendTarget::Wormhole => Protocol::Wormhole,
                    SendTarget::Bluetooth { .. } => Protocol::Bluetooth,
                };
                let adapter = self.adapter(protocol)?;
                let items = prepare(items).await?;
                adapter.send(target, items).await.map(Some)
            }
            Command::ReceiveWormhole { code } => {
                let adapter = self.adapter(Protocol::Wormhole)?;
                adapter.receive_code(code).await.map(Some)
            }
            Command::Cancel { transfer } => {
                if self.ctx.transfers().cancel(transfer) {
                    Ok(None)
                } else {
                    Err(ErrorInfo::new(ErrorCode::NotFound, "no such transfer"))
                }
            }
            Command::ListBluetoothDevices => {
                let adapter = self.adapter(Protocol::Bluetooth)?;
                let devices = adapter.list_devices().await?;
                self.ctx.emit(Event::BluetoothDevices { devices });
                Ok(None)
            }
        }
    }

    async fn set_receiving(&self, on: bool) {
        let mut state = self.state.lock().await;
        state.receiving = on;
        state.statuses = self.statuses(on);
        if on {
            self.emit_receiving(&state);
        }
        let settings = self.ctx.settings();
        for a in &self.adapters {
            if !a.receives() {
                continue;
            }
            let protocol = a.protocol();
            let result = if on && enabled(&settings, protocol) {
                a.start_receiving().await.map(|()| ProtocolState::Ready)
            } else {
                a.stop_receiving().await;
                Ok(ProtocolState::Off)
            };
            if let Some(s) = state.statuses.iter_mut().find(|s| s.protocol == protocol) {
                match result {
                    Ok(st) => {
                        s.state = st;
                        s.error = None;
                    }
                    Err(e) => {
                        s.state = ProtocolState::Failed;
                        s.error = Some(e);
                    }
                }
            }
        }
        self.emit_receiving(&state);
    }

    fn statuses(&self, on: bool) -> Vec<ProtocolStatus> {
        let settings = self.ctx.settings();
        self.adapters
            .iter()
            .map(|a| {
                let state = if !a.receives() {
                    ProtocolState::SendOnly
                } else if on && enabled(&settings, a.protocol()) {
                    ProtocolState::Starting
                } else {
                    ProtocolState::Off
                };
                ProtocolStatus {
                    protocol: a.protocol(),
                    state,
                    error: None,
                }
            })
            .collect()
    }

    fn emit_receiving(&self, state: &HubState) {
        self.ctx.emit(Event::Receiving {
            on: state.receiving,
            protocols: state.statuses.clone(),
        });
    }

    fn emit_settings(&self) {
        self.ctx.emit(Event::Settings {
            settings: self.ctx.settings(),
            effective_device_name: self.ctx.device_name(),
        });
    }

    fn adapter(&self, protocol: Protocol) -> Result<Arc<dyn Adapter>, ErrorInfo> {
        let settings = self.ctx.settings();
        if !enabled(&settings, protocol) {
            return Err(ErrorInfo::new(
                ErrorCode::Unavailable,
                "the protocol is disabled in the settings",
            ));
        }
        self.adapters
            .iter()
            .find(|a| a.protocol() == protocol)
            .cloned()
            .ok_or_else(|| {
                ErrorInfo::new(ErrorCode::Unavailable, "the protocol is not in this build")
            })
    }

    fn enabled_adapters(&self) -> Vec<Arc<dyn Adapter>> {
        let settings = self.ctx.settings();
        self.adapters
            .iter()
            .filter(|a| enabled(&settings, a.protocol()))
            .cloned()
            .collect()
    }
}

fn enabled(settings: &Settings, protocol: Protocol) -> bool {
    match protocol {
        Protocol::LocalSend => settings.localsend.enabled,
        Protocol::QuickShare => settings.quickshare.enabled,
        Protocol::Wormhole => true,
        Protocol::Bluetooth => settings.bluetooth.enabled,
    }
}

#[allow(clippy::vec_init_then_push, unused_mut)] // One push per feature.
fn build_adapters(ctx: &Arc<Ctx>) -> Vec<Arc<dyn Adapter>> {
    let mut adapters: Vec<Arc<dyn Adapter>> = Vec::new();
    #[cfg(feature = "localsend")]
    adapters.push(crate::localsend::adapter(ctx.clone()));
    #[cfg(feature = "quickshare")]
    adapters.push(crate::quickshare::adapter(ctx.clone()));
    #[cfg(feature = "wormhole")]
    adapters.push(crate::wormhole::adapter(ctx.clone()));
    #[cfg(feature = "bluetooth")]
    adapters.push(crate::bluetooth::adapter(ctx.clone()));
    let _ = ctx;
    adapters
}

fn consent_observer(events: EventSink) -> sukkula_core::consent::Observer {
    Arc::new(move |e| match e {
        ConsentEvent::Pending { id, offer } => {
            let files: Vec<FileView> = offer
                .files
                .iter()
                .take(MAX_LISTED_FILES)
                .map(|f| FileView {
                    name: f.name.as_str().to_owned(),
                    size: f.size,
                })
                .collect();
            let more_files = offer.files.len().saturating_sub(files.len());
            events(Event::OfferPending {
                offer: OfferView {
                    id,
                    protocol: offer.protocol,
                    sender: offer.sender.clone(),
                    model: offer.model.clone(),
                    files,
                    more_files,
                    file_count: offer.files.len(),
                    total_bytes: offer.total_bytes,
                    has_text: offer.text.is_some(),
                    pin: offer.pin.clone(),
                    expires_in: OFFER_TIMEOUT.as_secs(),
                },
            });
        }
        ConsentEvent::Closed { id, reason } => events(Event::OfferClosed { offer: id, reason }),
    })
}

fn load_settings(store: &Store) -> Settings {
    match store.read_json::<Settings>(SETTINGS_FILE, MAX_SETTINGS_BYTES) {
        Ok(Some(s)) => s.validate().unwrap_or_default(),
        Ok(None) => Settings::default(),
        Err(e) => {
            tracing::warn!(error = %e, "settings unreadable; using defaults");
            Settings::default()
        }
    }
}

fn absolute(p: &str) -> Result<PathBuf, ErrorInfo> {
    let path = PathBuf::from(p);
    if !path.is_absolute() || p.contains('\0') {
        return Err(ErrorInfo::new(
            ErrorCode::BadCommand,
            "paths must be absolute",
        ));
    }
    Ok(path)
}

/// The model from `/etc/hw-release` (`NAME=`), after S2.
fn read_hw_model() -> String {
    use std::io::Read as _;
    let mut buf = String::new();
    let read = std::fs::File::open("/etc/hw-release")
        .and_then(|f| f.take(MAX_HW_RELEASE_BYTES).read_to_string(&mut buf));
    if read.is_err() {
        return String::new();
    }
    let name = buf
        .lines()
        .find_map(|l| l.strip_prefix("NAME="))
        .map(|v| v.trim().trim_matches('"'))
        .unwrap_or("");
    text::display(name, MAX_MODEL_CHARS)
}

/// Checks everything a `send` names before any adapter sees it.
async fn prepare(items: Vec<SendItem>) -> Result<Vec<Outgoing>, ErrorInfo> {
    if items.is_empty() {
        return Err(ErrorInfo::new(ErrorCode::BadCommand, "nothing to send"));
    }
    if items.len() > MAX_FILES_PER_OFFER {
        return Err(ErrorInfo::new(ErrorCode::TooLarge, "too many files"));
    }
    let mut total: u64 = 0;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            SendItem::Text { text } => {
                if text.len() > MAX_MESSAGE_BYTES {
                    return Err(ErrorInfo::new(ErrorCode::TooLarge, "the text is too long"));
                }
                if text.is_empty() {
                    return Err(ErrorInfo::new(ErrorCode::BadCommand, "empty text"));
                }
                out.push(Outgoing::Text(text));
            }
            SendItem::File { path } => {
                let file = check_file(Path::new(&path)).await?;
                total = total
                    .checked_add(file.size)
                    .filter(|t| *t <= MAX_OFFER_BYTES)
                    .ok_or_else(|| {
                        ErrorInfo::new(ErrorCode::TooLarge, "the files are too large together")
                    })?;
                out.push(Outgoing::File(file));
            }
        }
    }
    Ok(out)
}

async fn check_file(path: &Path) -> Result<OutgoingFile, ErrorInfo> {
    let bad = |m: &str| ErrorInfo::new(ErrorCode::BadFile, m.to_owned());
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(bad("the path is not absolute"));
    }
    // Follows symlinks on purpose: the picker hands out links. What matters
    // is that the far end is a regular file -- never a FIFO that would
    // block a read forever, a device, or a directory.
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|_| bad("the file cannot be read"))?;
    if !meta.is_file() {
        return Err(bad("not a regular file"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(ErrorInfo::new(ErrorCode::TooLarge, "the file is too large"));
    }
    let raw_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = name::sanitize(&raw_name);
    let mime = guess_mime(name.as_str());
    Ok(OutgoingFile {
        path: path.to_path_buf(),
        name,
        size: meta.len(),
        mime,
    })
}

/// A MIME type from the extension, for the few types receivers care about.
fn guess_mime(name: &str) -> Option<String> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    let m = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "txt" => "text/plain",
        "vcf" => "text/vcard",
        "apk" => "application/vnd.android.package-archive",
        _ => return None,
    };
    Some(m.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::RequestId;
    use std::sync::Mutex;

    fn start_engine(dir: &Path) -> (Engine, Arc<Mutex<Vec<Event>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        let sink: EventSink = Arc::new(move |e| l.lock().unwrap().push(e));
        let cfg = StartConfig {
            v: API_VERSION,
            data_dir: dir.join("data").to_string_lossy().into_owned(),
            download_dir: dir.join("dl").to_string_lossy().into_owned(),
            device_model: Some("Test Phone".into()),
            allow_loopback: true,
        };
        (Engine::start(cfg, sink).unwrap(), log)
    }

    fn wait_for_reply(log: &Arc<Mutex<Vec<Event>>>, id: RequestId) -> Event {
        for _ in 0..500 {
            if let Some(e) = log
                .lock()
                .unwrap()
                .iter()
                .find(|e| matches!(e, Event::Reply { id: i, .. } if *i == id))
            {
                return e.clone();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("no reply to {id}");
    }

    #[test]
    fn start_emits_started_settings_receiving() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, log) = start_engine(dir.path());
        {
            let log = log.lock().unwrap();
            assert!(matches!(
                log[0],
                Event::Started {
                    api: API_VERSION,
                    ..
                }
            ));
            assert!(
                matches!(&log[1], Event::Settings { effective_device_name, .. } if effective_device_name == "Test Phone")
            );
            assert!(matches!(log[2], Event::Receiving { on: false, .. }));
        }
        engine.stop();
    }

    #[test]
    fn malformed_commands_get_a_reply() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, log) = start_engine(dir.path());
        engine.command_json(r#"{"v":1,"id":5,"cmd":{"type":"nope"}}"#);
        assert!(matches!(
            wait_for_reply(&log, 5),
            Event::Reply { ok: false, .. }
        ));
        engine.command_json(r#"{"v":1,"id":6,"cmd":{"type":"answer","offer":42,"accept":true}}"#);
        match wait_for_reply(&log, 6) {
            Event::Reply {
                ok: false,
                error: Some(e),
                ..
            } => assert_eq!(e.code, ErrorCode::NotFound),
            other => panic!("{other:?}"),
        }
        engine.stop();
    }

    #[test]
    fn settings_round_trip_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, log) = start_engine(dir.path());
        engine.command_json(
            r#"{"v":1,"id":1,"cmd":{"type":"set_settings","settings":{"device_name":"Pekka","logging":false}}}"#,
        );
        assert!(matches!(
            wait_for_reply(&log, 1),
            Event::Reply { ok: true, .. }
        ));
        engine.stop();
        let (engine, log) = start_engine(dir.path());
        assert!(
            matches!(&log.lock().unwrap()[1], Event::Settings { effective_device_name, .. } if effective_device_name == "Pekka")
        );
        engine.command_json(
            r#"{"v":1,"id":2,"cmd":{"type":"set_settings","settings":{"localsend":{"enabled":true,"pin":"no spaces"}}}}"#,
        );
        match wait_for_reply(&log, 2) {
            Event::Reply {
                ok: false,
                error: Some(e),
                ..
            } => assert_eq!(e.code, ErrorCode::BadSettings),
            other => panic!("{other:?}"),
        }
        engine.stop();
    }

    #[test]
    fn nothing_is_emitted_after_stop() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, log) = start_engine(dir.path());
        engine.stop();
        let n = log.lock().unwrap().len();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(log.lock().unwrap().len(), n);
    }

    #[tokio::test]
    async fn sends_are_checked_before_any_adapter_sees_them() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let e = prepare(vec![SendItem::File {
            path: "relative".into(),
        }])
        .await
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::BadFile);
        let e = prepare(vec![SendItem::File {
            path: dir.path().to_string_lossy().into_owned(),
        }])
        .await
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::BadFile, "a directory");
        let e = prepare(vec![SendItem::File {
            path: fifo.to_string_lossy().into_owned(),
        }])
        .await
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::BadFile, "missing");
        let e = prepare(vec![]).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::BadCommand);
        let e = prepare(vec![SendItem::Text {
            text: "x".repeat(MAX_MESSAGE_BYTES + 1),
        }])
        .await
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::TooLarge);
        let ok = prepare(vec![SendItem::Text { text: "hi".into() }])
            .await
            .unwrap();
        assert_eq!(ok, vec![Outgoing::Text("hi".into())]);
    }
}
