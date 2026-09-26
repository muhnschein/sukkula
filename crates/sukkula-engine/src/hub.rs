//! The hub: parses commands, drives the adapters, answers every command
//! with exactly one `Reply`, and hands events to the sink.
//!
//! # Delivery
//!
//! Nothing in the engine calls the sink directly. Every event goes into the
//! [`Gate`], a bounded FIFO, and one thread of the engine's own
//! (`sukkula-events`) takes events out and calls the sink with them, one at
//! a time. That buys the C ABI its promises:
//!
//! - the sink is never called on a thread that called into the engine, so
//!   a shell may post to its UI thread without fearing re-entrancy;
//! - the sink is never called twice at once, and events arrive in the
//!   order they were emitted;
//! - a slow UI slows the engine only once the queue is full, and a full
//!   queue slows only the tasks that emit, never the one that delivers;
//! - after [`Engine::stop`] returns the sink is never called again.
//!
//! The queue is bounded by count and by bytes ([`MAX_QUEUED_EVENTS`],
//! [`MAX_QUEUED_BYTES`]). An emitter that finds it full waits for room,
//! with two exceptions that keep waiting from ever turning into a
//! deadlock: a `Reply`, which holds one of the [`MAX_IN_FLIGHT_COMMANDS`]
//! command slots and is therefore bounded already, and anything emitted on
//! the delivery thread itself (a sink that sends a command), which would be
//! waiting for itself.
//!
//! # Commands
//!
//! A command takes a slot before it is parsed and gives it back when its
//! reply has been delivered, so at most [`MAX_IN_FLIGHT_COMMANDS`] commands
//! and their replies exist at once; the next is refused as
//! [`Refused::Busy`] without a reply. Each runs in a task of its own under
//! a time limit ([`command_timeout`]). A [`ReplyGuard`] owns the slot:
//! whatever becomes of the task -- it finishes, it panics, it times out,
//! the runtime drops it at stop -- exactly one `Reply` is emitted for it.

use std::cell::Cell;
use std::collections::VecDeque;
use std::future::Future;
use std::io::{self, Read as _};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{JoinHandle, ThreadId};
use std::time::Duration;

use sukkula_core::Protocol;
use sukkula_core::config::{MAX_SETTINGS_BYTES, SETTINGS_FILE, Settings};
use sukkula_core::consent::{ConsentBroker, ConsentEvent, Decision};
use sukkula_core::inbox::Inbox;
use sukkula_core::limits::{
    HANDSHAKE_TIMEOUT, MAX_EVENT_BYTES, MAX_FILE_BYTES, MAX_FILES_PER_OFFER, MAX_MESSAGE_BYTES,
    MAX_MODEL_CHARS, MAX_OFFER_BYTES, MAX_PEERS, NETWORK_IDLE_TIMEOUT, OFFER_TIMEOUT,
};
use sukkula_core::name;
use sukkula_core::reach::ReachPolicy;
use sukkula_core::store::{Store, StoreError};
use sukkula_core::text;
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};
use tokio_util::sync::CancellationToken;

use crate::adapter::{Adapter, Outgoing, OutgoingFile};
use crate::api::{
    API_VERSION, Command, CommandEnvelope, ErrorCode, ErrorInfo, Event, FileView,
    MAX_IN_FLIGHT_COMMANDS, MAX_LISTED_FILES, OfferView, Outcome, ProtocolState, ProtocolStatus,
    RequestId, SendItem, SendTarget, StartConfig, TransferId, parse_command,
};
use crate::ctx::{Ctx, EventSink};
use crate::logging::{self, LogSink, Logging};

/// How long [`Engine::stop`] waits for adapters and tasks to wind down, per
/// phase.
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// How long one adapter may take to start or stop receiving or discovery.
/// Past it the adapter is reported as failed and the command goes on, so
/// one hung adapter cannot hold the receive switch hostage.
const ADAPTER_TIMEOUT: Duration = Duration::from_secs(15);

/// The most one command may take. A last resort behind the adapters' own
/// timeouts: without it one hung adapter call would hold a command slot
/// for the life of the engine.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// The most a `receive_wormhole` may take: 150 s. It is answered only once
/// the user has answered the offer (F-MW2), so it waits for the network and
/// then for the user, each bounded by the adapter: the mailbox connection
/// and the key exchange ([`HANDSHAKE_TIMEOUT`] each), the offer
/// ([`NETWORK_IDLE_TIMEOUT`]), the consent dialog's full [`OFFER_TIMEOUT`],
/// and a goodbye, with room to spare. Under [`COMMAND_TIMEOUT`] the command
/// died before the dialog's countdown did ("Wormhole receive consent is
/// killed by the 60 s command timeout").
const RECEIVE_CODE_TIMEOUT: Duration = HANDSHAKE_TIMEOUT
    .saturating_mul(2)
    .saturating_add(NETWORK_IDLE_TIMEOUT)
    .saturating_add(OFFER_TIMEOUT)
    .saturating_add(Duration::from_secs(20));

/// Most events waiting for the sink, not counting replies (which are
/// bounded by [`MAX_IN_FLIGHT_COMMANDS`]).
const MAX_QUEUED_EVENTS: usize = 1024;

/// Most bytes of events waiting for the sink, measured as JSON, not
/// counting replies.
const MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;

/// Where Sailfish describes the hardware.
const HW_RELEASE: &str = "/etc/hw-release";

/// Largest `/etc/hw-release` read.
const MAX_HW_RELEASE_BYTES: u64 = 4096;

/// Longest configured directory path, in bytes (Linux's `PATH_MAX`).
const MAX_PATH_BYTES: usize = 4096;

/// A running engine.
pub struct Engine {
    runtime: Option<tokio::runtime::Runtime>,
    hub: Arc<Hub>,
    delivery: Delivery,
}

/// Why [`Engine::try_command`] did not take a command. No reply follows.
// CONTRACT: new (additive), with Engine::try_command{,_json}; re-exported
// from the crate root. Engine's existing methods keep their signatures.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum Refused {
    /// [`MAX_IN_FLIGHT_COMMANDS`] commands are waiting for their replies.
    #[error("too many commands are waiting for their reply")]
    Busy,
    /// The engine is stopping.
    #[error("the engine is stopping")]
    Stopped,
}

struct Hub {
    ctx: Arc<Ctx>,
    adapters: Vec<Arc<dyn Adapter>>,
    gate: Arc<Gate>,
    /// Commands taken and not yet answered to the UI.
    in_flight: Arc<AtomicUsize>,
    /// The receive switch. Held across a whole `set_receiving` or
    /// `set_settings`, so the two never interleave.
    state: AsyncMutex<HubState>,
    /// Held across a whole `start_discovery` or `stop_discovery`.
    discovery: AsyncMutex<()>,
    /// This engine's log (S9), switched by `Settings::logging`.
    logging: Logging,
    /// The settings file was not used as it was saved; the UI is told with
    /// every `settings` event until a `set_settings` replaces it.
    recovered: AtomicBool,
}

struct HubState {
    receiving: bool,
    statuses: Vec<ProtocolStatus>,
}

impl Engine {
    /// Starts an engine. Emits `Started`, `Settings` and `Receiving`, in that
    /// order, before any other event, and returns only once the sink has
    /// had all three.
    ///
    /// Safe to call from inside another tokio runtime.
    ///
    /// # Errors
    ///
    /// The configuration is unusable: another API version; a path that is
    /// relative, too long, has `.` or `..` components or control
    /// characters; a data directory and download directory that are the
    /// same or nested; a directory that cannot be created; or a thread or
    /// runtime that cannot start. The sink is not called.
    ///
    /// The engine's log goes to standard error (see [`logging`]).
    pub fn start(config: StartConfig, sink: EventSink) -> Result<Engine, ErrorInfo> {
        Self::start_with_log(config, sink, logging::stderr())
    }

    /// Starts an engine whose log lines go to `log` instead of standard
    /// error, with the same filter and format. For tests, which read the
    /// log back; the C ABI always uses [`start`](Self::start).
    ///
    /// # Errors
    ///
    /// As for [`start`](Self::start).
    // CONTRACT: new (additive); `start` keeps its signature.
    pub fn start_with_log(
        config: StartConfig,
        sink: EventSink,
        log: LogSink,
    ) -> Result<Engine, ErrorInfo> {
        // Off until the settings say otherwise: whatever the start itself
        // logs is held to the default.
        let logging = Logging::new(log, false);
        logging.scope(|| Self::start_logged(config, sink, logging.clone()))
    }

    fn start_logged(
        config: StartConfig,
        sink: EventSink,
        logging: Logging,
    ) -> Result<Engine, ErrorInfo> {
        if config.v != API_VERSION {
            return Err(ErrorInfo::new(
                ErrorCode::BadVersion,
                "unsupported API version",
            ));
        }
        let data_dir = checked_dir(&config.data_dir, "data_dir")?;
        let download_dir = checked_dir(&config.download_dir, "download_dir")?;
        apart(&data_dir, &download_dir)?;
        let store = Store::open(&data_dir)
            .map_err(|e| ErrorInfo::new(ErrorCode::Storage, e.to_string()))?;
        let inbox = Inbox::open(&download_dir)
            .map_err(|e| ErrorInfo::new(ErrorCode::Storage, e.to_string()))?;
        // Again, now that both exist: a symlink could make two different
        // strings one directory.
        apart(&resolved(&data_dir)?, &resolved(&download_dir)?)?;
        let (settings, recovered) = load_settings(&store);
        logging.set(settings.logging);
        let model = config
            .device_model
            .map_or_else(read_hw_model, |m| text::display(&m, MAX_MODEL_CHARS));

        let delivery = Delivery::spawn(sink, &logging).map_err(|e| internal(&e))?;
        let events: EventSink = {
            let gate = delivery.gate.clone();
            Arc::new(move |e| {
                gate.emit(e);
            })
        };
        let consent = ConsentBroker::new(consent_observer(events.clone()));
        let reach = ReachPolicy {
            allow_loopback: config.allow_loopback,
        };
        let ctx = Arc::new(Ctx::new(
            settings, model, store, inbox, consent, reach, events,
        ));

        let on_start = logging.clone();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("sukkula-engine")
            // Workers and blocking threads alike log to this engine's log.
            .on_thread_start(move || on_start.enter_thread())
            .on_thread_stop(Logging::leave_thread)
            .enable_all()
            .build()
            .map_err(|e| internal(&e))?;

        let adapters = {
            let _guard = runtime.enter();
            build_adapters(&ctx)
        };
        let statuses = statuses(&adapters, &ctx.settings(), false);
        let hub = Arc::new(Hub {
            ctx,
            adapters,
            gate: delivery.gate.clone(),
            in_flight: Arc::new(AtomicUsize::new(0)),
            state: AsyncMutex::new(HubState {
                receiving: false,
                statuses: statuses.clone(),
            }),
            discovery: AsyncMutex::new(()),
            logging,
            recovered: AtomicBool::new(recovered),
        });

        hub.ctx.emit(Event::Started {
            version: crate::VERSION.to_owned(),
            api: API_VERSION,
            protocols: hub.adapters.iter().map(|a| a.protocol()).collect(),
        });
        hub.emit_settings();
        let receiving = Event::Receiving {
            on: false,
            protocols: statuses,
        };
        if let Some(seq) = hub.gate.push(receiving, None) {
            hub.gate.wait_delivered(seq);
        }
        Ok(Engine {
            runtime: Some(runtime),
            hub,
            delivery,
        })
    }

    /// Takes one command as JSON. Always answered by exactly one `Reply`,
    /// even when the JSON is malformed or the engine is busy (then with
    /// `too_large`); only a stopping engine does not answer.
    ///
    /// For Rust callers and tests. The C ABI uses
    /// [`try_command_json`](Self::try_command_json), which refuses a
    /// command it is too busy for instead of answering it.
    pub fn command_json(&self, json: &str) {
        if self.try_command_json(json) == Err(Refused::Busy) {
            let id = match parse_command(json) {
                Ok(env) => env.id,
                Err((id, _)) => id.unwrap_or(0),
            };
            self.hub.logging.scope(|| self.hub.ctx.emit(busy_reply(id)));
        }
    }

    /// Takes one parsed command, as [`command_json`](Self::command_json).
    pub fn command(&self, env: CommandEnvelope) {
        let id = env.id;
        if self.try_command(env) == Err(Refused::Busy) {
            self.hub.logging.scope(|| self.hub.ctx.emit(busy_reply(id)));
        }
    }

    /// Takes one command as JSON, if there is room for it. When this
    /// returns `Ok`, exactly one `Reply` follows, from the delivery thread;
    /// otherwise none does.
    ///
    /// Never blocks. The JSON is parsed on the calling thread; a malformed
    /// command is answered like any other.
    ///
    /// # Errors
    ///
    /// [`Refused`].
    pub fn try_command_json(&self, json: &str) -> Result<(), Refused> {
        let runtime = self.runtime.as_ref().ok_or(Refused::Stopped)?;
        let permit = Permit::acquire(&self.hub.in_flight).ok_or(Refused::Busy)?;
        self.hub.logging.scope(|| match parse_command(json) {
            Ok(env) => self.hub.spawn_command(runtime, env, permit),
            Err((id, e)) => {
                self.hub
                    .gate
                    .push(reply_event(id.unwrap_or(0), Err(e.into())), Some(permit));
            }
        });
        Ok(())
    }

    /// Takes one parsed command, as [`try_command_json`](Self::try_command_json).
    ///
    /// # Errors
    ///
    /// [`Refused`].
    pub fn try_command(&self, env: CommandEnvelope) -> Result<(), Refused> {
        let runtime = self.runtime.as_ref().ok_or(Refused::Stopped)?;
        let permit = Permit::acquire(&self.hub.in_flight).ok_or(Refused::Busy)?;
        self.hub
            .logging
            .scope(|| self.hub.spawn_command(runtime, env, permit));
        Ok(())
    }

    /// Stops everything. When this returns, the sink will never be called
    /// again, and no engine thread is running.
    ///
    /// Callable from anywhere, including from inside the sink and from
    /// inside another tokio runtime:
    ///
    /// - From inside this engine's own sink, the gate closes first and
    ///   events still queued are dropped; the delivery thread, which is the
    ///   caller, ends as soon as the sink returns.
    /// - From inside another engine's sink, this engine's delivery thread
    ///   is waited for at most 3 seconds (`STOP_TIMEOUT`): the two sinks could be
    ///   waiting for each other.
    /// - Otherwise, events emitted before the gate closes -- including
    ///   those the teardown itself emits -- are delivered first.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let logging = self.hub.logging.clone();
        logging.scope(|| self.shutdown_logged());
    }

    fn shutdown_logged(&mut self) {
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        if self.delivery.is_current() {
            // This thread is the one that would drain the queue.
            self.delivery.finish(Finish::Discard);
        }
        let hub = self.hub.clone();
        let mut slot = Some(runtime);
        let teardown = |slot: &mut Option<tokio::runtime::Runtime>| {
            if let Some(runtime) = slot.take() {
                runtime.block_on(async {
                    let _ = tokio::time::timeout(STOP_TIMEOUT, hub.stop_adapters()).await;
                });
                hub.ctx.shut_down();
                runtime.shutdown_timeout(STOP_TIMEOUT);
            }
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            // Inside a runtime, `block_on` and a blocking runtime shutdown
            // both panic. Do them on a thread that is not.
            let spawned = std::thread::scope(|s| {
                std::thread::Builder::new()
                    .name("sukkula-stop".to_owned())
                    .spawn_scoped(s, || hub.logging.scope(|| teardown(&mut slot)))
                    .map(|t| t.join().is_ok())
            });
            if !matches!(spawned, Ok(true))
                && let Some(runtime) = slot.take()
            {
                hub.ctx.shut_down();
                runtime.shutdown_background();
            }
        } else {
            teardown(&mut slot);
        }
        let finish = if in_delivery() {
            Finish::Discard
        } else {
            Finish::Drain
        };
        self.delivery.finish(finish);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Hub {
    fn spawn_command(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Runtime,
        env: CommandEnvelope,
        permit: Permit,
    ) {
        let hub = self.clone();
        let limit = command_timeout(&env.cmd);
        let work = async move { hub.dispatch(env.cmd).await };
        spawn_reply(
            runtime.handle(),
            ReplyGuard::new(
                self.gate.clone(),
                env.id,
                permit,
                self.ctx.shutdown_token().clone(),
            ),
            limit,
            work,
        );
    }

    async fn dispatch(self: &Arc<Self>, cmd: Command) -> Result<Option<TransferId>, ErrorInfo> {
        match cmd {
            Command::SetReceiving { on } => {
                let mut state = self.switch().await?;
                self.set_receiving(&mut state, on).await;
                Ok(None)
            }
            Command::SetSettings { settings } => {
                let settings = settings
                    .validate()
                    .map_err(|e| ErrorInfo::new(ErrorCode::BadSettings, e.to_string()))?;
                // Held from the write to the restart: two `set_settings`
                // cannot leave the file and the engine disagreeing, and a
                // `set_receiving` cannot slip between a stop and a start.
                let mut state = self.switch().await?;
                let store = self.ctx.store().clone();
                let saved = settings.clone();
                let logging = settings.logging;
                tokio::task::spawn_blocking(move || store.write_json(SETTINGS_FILE, &saved))
                    .await
                    .map_err(|_| ErrorInfo::new(ErrorCode::Internal, "the settings write failed"))?
                    .map_err(|e| ErrorInfo::new(ErrorCode::Storage, e.to_string()))?;
                self.recovered.store(false, Ordering::Release);
                self.ctx.set_settings(settings);
                self.logging.set(logging);
                self.emit_settings();
                if state.receiving {
                    self.restart_receiving(&mut state).await;
                }
                Ok(None)
            }
            Command::GetSettings => {
                self.emit_settings();
                Ok(None)
            }
            Command::StartDiscovery => {
                let _held = self.discovery.lock().await;
                let settings = self.ctx.settings();
                let started = each(&self.adapters, |a| {
                    let on = enabled(&settings, a.protocol());
                    async move {
                        if on {
                            a.start_discovery().await
                        } else {
                            a.stop_discovery().await;
                            Ok(())
                        }
                    }
                })
                .await;
                for (protocol, result) in started {
                    if let Err(e) = result {
                        tracing::debug!(?protocol, code = ?e.code, "discovery failed to start");
                    }
                }
                Ok(None)
            }
            Command::StopDiscovery => {
                let _held = self.discovery.lock().await;
                each(&self.adapters, |a| async move {
                    a.stop_discovery().await;
                    Ok(())
                })
                .await;
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
                let mut devices = adapter.list_devices().await?;
                // The adapter bounds the list; this is the second layer.
                devices.truncate(MAX_PEERS);
                self.ctx.emit(Event::BluetoothDevices { devices });
                Ok(None)
            }
        }
    }

    /// Takes the receive switch for a `set_receiving` or `set_settings`,
    /// waiting at most [`COMMAND_TIMEOUT`] for the commands before it. Past
    /// that the command is refused having changed nothing.
    ///
    /// These two commands are not under a timeout as a whole (see
    /// [`command_timeout`]): once one has the switch, it runs to the end, so
    /// the final `receiving` is always emitted and the statuses are never
    /// left at `starting`. That end is bounded all the same: every protocol
    /// start or stop by [`ADAPTER_TIMEOUT`], a `set_settings` by two of
    /// them and a write of the settings file. A timeout around the whole
    /// could stop it half-way, with the switch turned and adapters started
    /// in tasks nobody waits for ("COMMAND_TIMEOUT can cancel
    /// set_receiving/set_settings mid-critical-section").
    async fn switch(&self) -> Result<AsyncMutexGuard<'_, HubState>, ErrorInfo> {
        tokio::time::timeout(COMMAND_TIMEOUT, self.state.lock())
            .await
            .map_err(|_| {
                ErrorInfo::new(
                    ErrorCode::Internal,
                    "the receive switch stayed busy; nothing was changed",
                )
            })
    }

    /// Turns the receivers on or off. Every receiving adapter is started or
    /// stopped at once, in a task of its own under [`ADAPTER_TIMEOUT`], so
    /// one that hangs or panics is reported as failed and the rest carry
    /// on.
    async fn set_receiving(&self, state: &mut HubState, on: bool) {
        let settings = self.ctx.settings();
        state.receiving = on;
        state.statuses = statuses(&self.adapters, &settings, on);
        if on {
            self.emit_receiving(state);
        }
        let receivers: Vec<Arc<dyn Adapter>> = self
            .adapters
            .iter()
            .filter(|a| a.receives())
            .cloned()
            .collect();
        let results = each(&receivers, |a| {
            let start = on && enabled(&settings, a.protocol());
            async move {
                if start {
                    a.start_receiving().await.map(|()| ProtocolState::Ready)
                } else {
                    a.stop_receiving().await;
                    Ok(ProtocolState::Off)
                }
            }
        })
        .await;
        for (protocol, result) in results {
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
        self.emit_receiving(state);
    }

    /// Restarts running receivers with new settings, without telling the
    /// UI the switch went off in between.
    async fn restart_receiving(&self, state: &mut HubState) {
        each(&self.adapters, |a| async move {
            if a.receives() {
                a.stop_receiving().await;
            }
            Ok(())
        })
        .await;
        self.set_receiving(state, true).await;
    }

    /// Stops discovery and receiving on every adapter, for [`Engine::stop`].
    async fn stop_adapters(&self) {
        each(&self.adapters, |a| async move {
            a.stop_discovery().await;
            a.stop_receiving().await;
            Ok(())
        })
        .await;
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
            recovered: self.recovered.load(Ordering::Acquire),
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
}

fn statuses(adapters: &[Arc<dyn Adapter>], settings: &Settings, on: bool) -> Vec<ProtocolStatus> {
    adapters
        .iter()
        .map(|a| {
            let state = if !a.receives() {
                ProtocolState::SendOnly
            } else if on && enabled(settings, a.protocol()) {
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

/// Runs `f` for every adapter at once, each in a task of its own under
/// [`ADAPTER_TIMEOUT`], and collects the results in adapter order. A task
/// that panics or times out is an error, not the caller's problem.
async fn each<T, F, Fut>(
    adapters: &[Arc<dyn Adapter>],
    f: F,
) -> Vec<(Protocol, Result<T, ErrorInfo>)>
where
    T: Send + 'static,
    F: Fn(Arc<dyn Adapter>) -> Fut,
    Fut: Future<Output = Result<T, ErrorInfo>> + Send + 'static,
{
    let tasks: Vec<_> = adapters
        .iter()
        .map(|a| {
            let work = f(a.clone());
            let task = tokio::spawn(async move {
                tokio::time::timeout(ADAPTER_TIMEOUT, work)
                    .await
                    .unwrap_or_else(|_| {
                        Err(ErrorInfo::new(
                            ErrorCode::Internal,
                            "the protocol did not answer in time",
                        ))
                    })
            });
            (a.protocol(), task)
        })
        .collect();
    let mut out = Vec::with_capacity(tasks.len());
    for (protocol, task) in tasks {
        let result = task.await.unwrap_or_else(|e| {
            if e.is_panic() {
                tracing::error!(?protocol, "adapter panicked");
            }
            Err(ErrorInfo::new(
                ErrorCode::Internal,
                "the protocol failed internally",
            ))
        });
        out.push((protocol, result));
    }
    out
}

/// Whether the settings let `protocol` run (F-C1): receive, discover, send,
/// and for wormhole receive by code.
fn enabled(settings: &Settings, protocol: Protocol) -> bool {
    match protocol {
        Protocol::LocalSend => settings.localsend.enabled,
        Protocol::QuickShare => settings.quickshare.enabled,
        Protocol::Wormhole => settings.wormhole.enabled,
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

/// The saved settings, and whether they were not usable as saved. A file
/// that does not read is not replaced by the defaults, which are the most
/// permissive settings there are: what cannot be read is switched off
/// (`Settings::from_stored`). The file is left as it is.
fn load_settings(store: &Store) -> (Settings, bool) {
    match store.read(SETTINGS_FILE, MAX_SETTINGS_BYTES) {
        Ok(Some(bytes)) => {
            let stored = Settings::from_stored(&bytes);
            if stored.unusable.is_empty() {
                return (stored.settings, false);
            }
            // S9: which parts, by their fixed names; never the file's
            // content, which holds the device name and the LocalSend PIN.
            tracing::warn!(
                parts = ?stored.unusable,
                "settings file not usable as saved; what could not be read is off"
            );
            (stored.settings, true)
        }
        Ok(None) => (Settings::default(), false),
        Err(e) => {
            // S9: only the kind. The error's text names the file's path.
            tracing::warn!(
                why = store_error_kind(&e),
                "settings file unreadable; every protocol is off"
            );
            (Settings::locked_down(), true)
        }
    }
}

/// What went wrong with a store file, without its contents or its path.
fn store_error_kind(e: &StoreError) -> &'static str {
    match e {
        StoreError::Directory(_) => "data directory unusable",
        StoreError::BadName => "bad file name",
        StoreError::NotRegular(_) => "not a regular file of ours",
        StoreError::Exposed(_) => "readable by other users",
        StoreError::TooLarge(_) => "too large",
        StoreError::Malformed(..) => "malformed",
        StoreError::Io(_) => "i/o error",
    }
}

fn internal(e: &io::Error) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Internal, e.to_string())
}

// ---------------------------------------------------------------------------
// Replies.

fn reply_event(id: RequestId, result: Result<Option<TransferId>, ErrorInfo>) -> Event {
    match result {
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
    }
}

fn busy_reply(id: RequestId) -> Event {
    reply_event(
        id,
        Err(ErrorInfo::new(
            ErrorCode::TooLarge,
            "too many commands are waiting for their reply",
        )),
    )
}

/// One of the [`MAX_IN_FLIGHT_COMMANDS`] command slots. Given back when
/// dropped, which for a command is after its reply has been delivered.
struct Permit(Arc<AtomicUsize>);

impl Permit {
    fn acquire(count: &Arc<AtomicUsize>) -> Option<Permit> {
        crate::slots::take(count, MAX_IN_FLIGHT_COMMANDS).then(|| Permit(count.clone()))
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        crate::slots::give_back(&self.0);
    }
}

/// Owns a command's slot and its reply. [`send`](Self::send) replies with
/// the result; dropping it unsent -- a panic, the runtime dropping the task
/// at stop -- replies with `internal`. Either way, once.
struct ReplyGuard {
    gate: Arc<Gate>,
    id: RequestId,
    permit: Option<Permit>,
    stopping: CancellationToken,
}

impl ReplyGuard {
    fn new(gate: Arc<Gate>, id: RequestId, permit: Permit, stopping: CancellationToken) -> Self {
        ReplyGuard {
            gate,
            id,
            permit: Some(permit),
            stopping,
        }
    }

    fn send(mut self, result: Result<Option<TransferId>, ErrorInfo>) {
        if let Some(permit) = self.permit.take() {
            self.gate.push(reply_event(self.id, result), Some(permit));
        }
    }
}

impl Drop for ReplyGuard {
    fn drop(&mut self) {
        if let Some(permit) = self.permit.take() {
            let message = if self.stopping.is_cancelled() {
                "the engine is stopping"
            } else {
                "the command failed internally"
            };
            let error = ErrorInfo::new(ErrorCode::Internal, message);
            self.gate
                .push(reply_event(self.id, Err(error)), Some(permit));
        }
    }
}

/// How long `cmd` may run before it is answered with `internal`: `None`
/// for a command bounded inside instead.
fn command_timeout(cmd: &Command) -> Option<Duration> {
    match cmd {
        // The wait for the switch is bounded, and so is every step after
        // it; see `Hub::switch`.
        Command::SetReceiving { .. } | Command::SetSettings { .. } => None,
        // It waits for the user too.
        Command::ReceiveWormhole { .. } => Some(RECEIVE_CODE_TIMEOUT),
        _ => Some(COMMAND_TIMEOUT),
    }
}

/// Runs one command's work, under `limit` if it has one, and replies
/// through `guard`.
fn spawn_reply<F>(
    runtime: &tokio::runtime::Handle,
    guard: ReplyGuard,
    limit: Option<Duration>,
    work: F,
) where
    F: Future<Output = Result<Option<TransferId>, ErrorInfo>> + Send + 'static,
{
    runtime.spawn(async move {
        let result = match limit {
            Some(limit) => tokio::time::timeout(limit, work).await.unwrap_or_else(|_| {
                Err(ErrorInfo::new(
                    ErrorCode::Internal,
                    "the command did not finish in time",
                ))
            }),
            None => work.await,
        };
        guard.send(result);
    });
}

// ---------------------------------------------------------------------------
// Delivery.

thread_local! {
    /// Set on every engine's delivery thread.
    static IN_DELIVERY: Cell<bool> = const { Cell::new(false) };
}

fn in_delivery() -> bool {
    IN_DELIVERY.with(Cell::get)
}

/// Stands between the engine and the sink: a bounded FIFO that one thread
/// drains into the sink. Once closed, nothing more reaches the sink.
struct Gate {
    queue: Mutex<Queue>,
    /// Wakes the delivery thread: an event arrived, or the gate closed.
    ready: Condvar,
    /// Wakes everyone else: room in the queue, an event delivered, the
    /// delivery thread gone, the gate closed.
    changed: Condvar,
}

struct Queue {
    items: VecDeque<Queued>,
    /// Events and bytes waiting that count against the bounds: all but
    /// replies holding a command slot.
    held_events: usize,
    held_bytes: usize,
    state: GateState,
    /// Events accepted so far, numbering them from 1.
    accepted: u64,
    /// Events the sink has returned from.
    delivered: u64,
    /// The delivery thread has exited.
    finished: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GateState {
    Open,
    /// No more events are taken; the queue is delivered, then the thread
    /// ends.
    Draining,
    /// No more events are taken or delivered.
    Closed,
}

struct Queued {
    event: Event,
    bytes: usize,
    /// A reply's command slot, given back once the reply is delivered.
    /// Also marks the event as not counting against the bounds.
    permit: Option<Permit>,
}

/// How [`Delivery::finish`] ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Finish {
    /// Deliver what is queued, then stop.
    Drain,
    /// Drop what is queued.
    Discard,
}

impl Gate {
    fn new() -> Gate {
        Gate {
            queue: Mutex::new(Queue {
                items: VecDeque::new(),
                held_events: 0,
                held_bytes: 0,
                state: GateState::Open,
                accepted: 0,
                delivered: 0,
                finished: false,
            }),
            ready: Condvar::new(),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn emit(&self, event: Event) {
        self.push(event, None);
    }

    /// Queues `event`, waiting for room unless it holds a command slot or
    /// this is the delivery thread. Returns its number, or `None` when the
    /// gate is closed or the event could not be made to fit
    /// [`MAX_EVENT_BYTES`].
    fn push(&self, event: Event, permit: Option<Permit>) -> Option<u64> {
        let (event, bytes) = fit(event)?;
        let counted = permit.is_none();
        let may_wait = counted && !in_delivery();
        let mut q = self.lock();
        while may_wait
            && q.state == GateState::Open
            && q.held_events > 0
            && (q.held_events >= MAX_QUEUED_EVENTS
                || q.held_bytes.saturating_add(bytes) > MAX_QUEUED_BYTES)
        {
            q = self.changed.wait(q).unwrap_or_else(PoisonError::into_inner);
        }
        if q.state != GateState::Open {
            return None;
        }
        if counted {
            q.held_events = q.held_events.saturating_add(1);
            q.held_bytes = q.held_bytes.saturating_add(bytes);
        }
        q.accepted = q.accepted.saturating_add(1);
        let seq = q.accepted;
        q.items.push_back(Queued {
            event,
            bytes,
            permit,
        });
        drop(q);
        self.ready.notify_one();
        Some(seq)
    }

    /// Stops taking events. Wakes every waiter.
    fn close(&self, finish: Finish) {
        let dropped = {
            let mut q = self.lock();
            match finish {
                Finish::Drain if q.state == GateState::Open => q.state = GateState::Draining,
                Finish::Drain => {}
                Finish::Discard => q.state = GateState::Closed,
            }
            if q.state == GateState::Closed {
                q.held_events = 0;
                q.held_bytes = 0;
                std::mem::take(&mut q.items)
            } else {
                VecDeque::new()
            }
        };
        // Dropped outside the lock: permits touch other state.
        drop(dropped);
        self.ready.notify_all();
        self.changed.notify_all();
    }

    /// Waits until event `seq` has been delivered, or never will be.
    fn wait_delivered(&self, seq: u64) {
        let q = self.lock();
        let _q = self
            .changed
            .wait_while(q, |q| {
                q.delivered < seq && q.state != GateState::Closed && !q.finished
            })
            .unwrap_or_else(PoisonError::into_inner);
    }

    /// Waits at most `timeout` for the delivery thread to end.
    fn wait_finished(&self, timeout: Duration) -> bool {
        let q = self.lock();
        let (q, _) = self
            .changed
            .wait_timeout_while(q, timeout, |q| !q.finished)
            .unwrap_or_else(PoisonError::into_inner);
        q.finished
    }

    /// The delivery thread's loop.
    fn run(&self, sink: EventSink) {
        IN_DELIVERY.with(|d| d.set(true));
        loop {
            let next = {
                let mut q = self.lock();
                loop {
                    if q.state == GateState::Closed {
                        break None;
                    }
                    if let Some(item) = q.items.pop_front() {
                        if item.permit.is_none() {
                            q.held_events = q.held_events.saturating_sub(1);
                            q.held_bytes = q.held_bytes.saturating_sub(item.bytes);
                        }
                        break Some(item);
                    }
                    if q.state == GateState::Draining {
                        break None;
                    }
                    q = self.ready.wait(q).unwrap_or_else(PoisonError::into_inner);
                }
            };
            let Some(Queued { event, permit, .. }) = next else {
                break;
            };
            // Room for a waiting emitter.
            self.changed.notify_all();
            if catch_unwind(AssertUnwindSafe(|| sink(event))).is_err() {
                tracing::error!("the event sink panicked");
            }
            drop(permit);
            {
                let mut q = self.lock();
                q.delivered = q.delivered.saturating_add(1);
            }
            self.changed.notify_all();
        }
        // Whatever the sink holds is released before anyone is told the
        // thread is done.
        drop(sink);
        let rest = {
            let mut q = self.lock();
            q.finished = true;
            std::mem::take(&mut q.items)
        };
        drop(rest);
        self.changed.notify_all();
    }
}

/// The delivery thread and its gate.
struct Delivery {
    gate: Arc<Gate>,
    thread: Option<JoinHandle<()>>,
    thread_id: ThreadId,
}

impl Delivery {
    fn spawn(sink: EventSink, logging: &Logging) -> io::Result<Delivery> {
        let gate = Arc::new(Gate::new());
        let g = gate.clone();
        let log = logging.clone();
        let thread = std::thread::Builder::new()
            .name("sukkula-events".to_owned())
            .spawn(move || {
                log.enter_thread();
                g.run(sink);
                Logging::leave_thread();
            })?;
        let thread_id = thread.thread().id();
        Ok(Delivery {
            gate,
            thread: Some(thread),
            thread_id,
        })
    }

    /// Whether this is the delivery thread.
    fn is_current(&self) -> bool {
        std::thread::current().id() == self.thread_id
    }

    /// Closes the gate and waits for the delivery thread, except where that
    /// could never end: on the delivery thread itself (it ends once the
    /// sink returns), and on another engine's delivery thread (bounded by
    /// [`STOP_TIMEOUT`]).
    fn finish(&mut self, finish: Finish) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        if self.is_current() {
            self.gate.close(Finish::Discard);
            return;
        }
        self.gate.close(finish);
        if in_delivery() && !self.gate.wait_finished(STOP_TIMEOUT) {
            tracing::warn!("the event sink did not return in time; not waiting for it");
            return;
        }
        let _ = thread.join();
    }
}

impl Drop for Delivery {
    fn drop(&mut self) {
        self.finish(Finish::Discard);
    }
}

/// Counts the bytes of JSON written, and fails past [`MAX_EVENT_BYTES`] so
/// an oversized event costs no more than the cap to measure.
struct Measure(usize);

impl io::Write for Measure {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len());
        if self.0 > MAX_EVENT_BYTES {
            return Err(io::Error::other("event too large"));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The event's size as JSON, if it is within [`MAX_EVENT_BYTES`].
fn encoded_len(event: &Event) -> Option<usize> {
    let mut m = Measure(0);
    serde_json::to_writer(&mut m, event).ok().map(|()| m.0)
}

/// Holds an event to [`MAX_EVENT_BYTES`]. Events are built from sanitised,
/// capped values and fit by construction; this is the check that the
/// construction is right. An event that does not fit is replaced where the
/// UI depends on it -- a reply must still answer its command, a transfer
/// must still end -- and dropped otherwise.
fn fit(event: Event) -> Option<(Event, usize)> {
    if let Some(n) = encoded_len(&event) {
        return Some((event, n));
    }
    let kind = event_kind(&event);
    tracing::error!(kind, "event over the size cap");
    let replacement = match event {
        Event::Reply { id, .. } => reply_event(
            id,
            Err(ErrorInfo::new(
                ErrorCode::Internal,
                "the reply was too large",
            )),
        ),
        Event::TransferFinished { transfer, .. } => Event::TransferFinished {
            transfer,
            outcome: Outcome::Failed {
                error: ErrorInfo::new(ErrorCode::Internal, "the report was too large"),
            },
            saved: Vec::new(),
        },
        _ => return None,
    };
    encoded_len(&replacement).map(|n| (replacement, n))
}

fn event_kind(event: &Event) -> &'static str {
    match event {
        Event::Fatal { .. } => "fatal",
        Event::Started { .. } => "started",
        Event::Reply { .. } => "reply",
        Event::Settings { .. } => "settings",
        Event::Receiving { .. } => "receiving",
        Event::PeerFound { .. } => "peer_found",
        Event::PeerLost { .. } => "peer_lost",
        Event::OfferPending { .. } => "offer_pending",
        Event::OfferClosed { .. } => "offer_closed",
        Event::TransferStarted { .. } => "transfer_started",
        Event::TransferProgress { .. } => "transfer_progress",
        Event::TransferFinished { .. } => "transfer_finished",
        Event::TextReceived { .. } => "text_received",
        Event::WormholeCode { .. } => "wormhole_code",
        Event::BluetoothDevices { .. } => "bluetooth_devices",
    }
}

// ---------------------------------------------------------------------------
// Start configuration.

/// A directory from the start configuration: absolute, at most
/// [`MAX_PATH_BYTES`], below the root, no `.` or `..` components (which
/// would make the overlap check below meaningless), no NUL or other
/// control characters.
fn checked_dir(p: &str, what: &str) -> Result<PathBuf, ErrorInfo> {
    let bad = |why: &str| ErrorInfo::new(ErrorCode::BadCommand, format!("{what} {why}"));
    let path = Path::new(p);
    if p.len() > MAX_PATH_BYTES {
        return Err(bad("is too long"));
    }
    if p.chars().any(char::is_control) {
        return Err(bad("contains control characters"));
    }
    if !path.is_absolute() {
        return Err(bad("must be absolute"));
    }
    if p.split('/').any(|c| c == "." || c == "..") {
        return Err(bad("must not contain . or .."));
    }
    if path.parent().is_none() {
        return Err(bad("must not be the root directory"));
    }
    Ok(path.to_path_buf())
}

/// The data directory holds the settings and the TLS key; the download
/// directory holds whatever peers send. Neither may be inside the other,
/// or a received file could land among the secrets (or the secrets in
/// `~/Downloads`).
fn apart(data: &Path, download: &Path) -> Result<(), ErrorInfo> {
    if data.starts_with(download) || download.starts_with(data) {
        return Err(ErrorInfo::new(
            ErrorCode::BadCommand,
            "data_dir and download_dir must be separate directories",
        ));
    }
    Ok(())
}

fn resolved(p: &Path) -> Result<PathBuf, ErrorInfo> {
    std::fs::canonicalize(p).map_err(|e| ErrorInfo::new(ErrorCode::Storage, e.to_string()))
}

/// The device model from `/etc/hw-release`, after S2; empty if unknown.
fn read_hw_model() -> String {
    read_hw_model_from(Path::new(HW_RELEASE))
}

fn read_hw_model_from(path: &Path) -> String {
    // A regular file only: opening a FIFO would block start until a writer
    // came along, and a device could be read forever.
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => {}
        _ => return String::new(),
    }
    let mut buf = Vec::new();
    let read =
        std::fs::File::open(path).and_then(|f| f.take(MAX_HW_RELEASE_BYTES).read_to_end(&mut buf));
    if read.is_err() {
        return String::new();
    }
    parse_hw_release(&String::from_utf8_lossy(&buf))
}

/// The model from `hw-release` (the `os-release(5)` format): `NAME=`, else
/// the adaptation's device name `MER_HA_DEVICE=`, after S2.
fn parse_hw_release(content: &str) -> String {
    let mut name = None;
    let mut device = None;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let slot = match key.trim() {
            "NAME" => &mut name,
            "MER_HA_DEVICE" => &mut device,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(unquote(value));
        }
    }
    [name, device]
        .into_iter()
        .flatten()
        .map(|v| text::display(&v, MAX_MODEL_CHARS))
        .find(|v| !v.is_empty())
        .unwrap_or_default()
}

/// An `os-release(5)` value: optionally in double quotes, with `\` escaping
/// the next character, or in single quotes, taken literally.
fn unquote(raw: &str) -> String {
    let raw = raw.trim();
    if let Some(inner) = raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return inner.to_owned();
    }
    let Some(inner) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return raw.to_owned();
    };
    let mut out = String::with_capacity(inner.len());
    let mut escaped = false;
    for c in inner.chars() {
        if escaped || c != '\\' {
            out.push(c);
            escaped = false;
        } else {
            escaped = true;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Sending.

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
#[allow(
    clippy::disallowed_methods, // Tests set the scene with plain writes.
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_possible_truncation
)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::time::Instant;
    use sukkula_core::consent::Closed;

    fn config(dir: &Path) -> StartConfig {
        StartConfig {
            v: API_VERSION,
            data_dir: dir.join("data").to_string_lossy().into_owned(),
            download_dir: dir.join("dl").to_string_lossy().into_owned(),
            device_model: Some("Test Phone".into()),
            allow_loopback: true,
        }
    }

    fn start_engine(dir: &Path) -> (Engine, Arc<Mutex<Vec<Event>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        let sink: EventSink = Arc::new(move |e| l.lock().unwrap().push(e));
        (Engine::start(config(dir), sink).unwrap(), log)
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

    fn recording_delivery() -> (Delivery, Arc<Mutex<Vec<Event>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        let sink: EventSink = Arc::new(move |e| l.lock().unwrap().push(e));
        (Delivery::spawn(sink, &quiet()).unwrap(), log)
    }

    /// A log that goes nowhere.
    fn quiet() -> Logging {
        Logging::new(Arc::new(|_| {}), false)
    }

    type Lines = Arc<Mutex<Vec<String>>>;

    /// An engine whose log lines are kept, and whose sink logs a warning
    /// from the delivery thread for every reply it is handed.
    fn start_logged_engine(dir: &Path) -> (Engine, Arc<Mutex<Vec<Event>>>, Lines) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let e = events.clone();
        let sink: EventSink = Arc::new(move |event| {
            if let Event::Reply { id, .. } = &event {
                tracing::warn!(id, "the delivery thread logs here");
            }
            e.lock().unwrap().push(event);
        });
        let lines: Lines = Arc::default();
        let l = lines.clone();
        let log: LogSink = Arc::new(move |line| l.lock().unwrap().push(line.to_owned()));
        let engine = Engine::start_with_log(config(dir), sink, log).unwrap();
        (engine, events, lines)
    }

    /// Logs a debug and a warning line from a runtime worker and from a
    /// blocking thread of `engine`, and waits for them.
    fn probe(engine: &Engine, tag: &'static str) {
        let rt = engine.runtime.as_ref().unwrap();
        rt.block_on(async move {
            tokio::spawn(async move {
                tracing::debug!(tag, "debug from a worker");
                tracing::warn!(tag, "warning from a worker");
            })
            .await
            .unwrap();
            tokio::task::spawn_blocking(move || {
                tracing::debug!(tag, "debug from a blocking thread");
                tracing::warn!(tag, "warning from a blocking thread");
            })
            .await
            .unwrap();
        });
    }

    fn with_tag(lines: &Lines, tag: &str) -> Vec<String> {
        let tag = format!("tag={tag:?}");
        lines
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains(&tag))
            .cloned()
            .collect()
    }

    #[test]
    fn every_engine_thread_logs_to_the_engine_log_and_the_setting_switches_it() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, events, lines) = start_logged_engine(dir.path());
        probe(&engine, "off");
        let off = with_tag(&lines, "off");
        assert_eq!(off.len(), 2, "{off:?}");
        assert!(
            off.iter().all(|l| l.starts_with("sukkula: WARN ")),
            "{off:?}"
        );

        engine.command_json(
            r#"{"v":1,"id":1,"cmd":{"type":"set_settings","settings":{"logging":true}}}"#,
        );
        wait_for_reply(&events, 1);
        probe(&engine, "on");
        let on = with_tag(&lines, "on");
        assert_eq!(on.len(), 4, "{on:?}");
        assert_eq!(
            on.iter()
                .filter(|l| l.starts_with("sukkula: DEBUG "))
                .count(),
            2
        );
        // The delivery thread's log is the engine's too.
        let delivered = "the delivery thread logs here id=1";
        wait_until("the delivery thread's line", || {
            lines.lock().unwrap().iter().any(|l| l.contains(delivered))
        });
        assert!(
            lines
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.contains("INFO") && l.contains("debug logging on"))
        );

        engine.command_json(
            r#"{"v":1,"id":2,"cmd":{"type":"set_settings","settings":{"logging":false}}}"#,
        );
        wait_for_reply(&events, 2);
        probe(&engine, "off again");
        assert_eq!(with_tag(&lines, "off again").len(), 2);
        engine.stop();
    }

    #[test]
    fn logging_on_is_remembered_and_two_engines_have_two_switches() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, events, _) = start_logged_engine(dir.path());
        engine.command_json(
            r#"{"v":1,"id":1,"cmd":{"type":"set_settings","settings":{"logging":true}}}"#,
        );
        wait_for_reply(&events, 1);
        engine.stop();

        let (loud, _, loud_lines) = start_logged_engine(dir.path());
        let other = tempfile::tempdir().unwrap();
        let (quiet, _, quiet_lines) = start_logged_engine(other.path());
        probe(&loud, "loud");
        probe(&quiet, "quiet");
        assert_eq!(with_tag(&loud_lines, "loud").len(), 4);
        assert_eq!(with_tag(&quiet_lines, "quiet").len(), 2);
        assert!(with_tag(&loud_lines, "quiet").is_empty());
        assert!(with_tag(&quiet_lines, "loud").is_empty());
        loud.stop();
        quiet.stop();
    }

    #[test]
    fn a_bad_settings_file_is_logged_by_kind_never_by_content() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        Store::open(&data).unwrap();
        // serde's message for this quotes the value: "invalid type:
        // integer `98765`, expected a string".
        std::fs::write(
            data.join(SETTINGS_FILE),
            r#"{"device_name":"Zorbulon","localsend":{"pin":98765}}"#,
        )
        .unwrap();
        let (engine, _, lines) = start_logged_engine(dir.path());
        engine.stop();
        let lines = lines.lock().unwrap().clone();
        assert!(
            lines.iter().any(|l| l.starts_with("sukkula: WARN ")
                && l.contains("settings file not usable as saved")
                && l.contains(r#"parts=["localsend"]"#)),
            "{lines:?}"
        );
        for l in &lines {
            assert!(!l.contains("98765") && !l.contains("Zorbulon"), "{l}");
        }
        // A file that is not ours to read at all: by kind too.
        std::fs::remove_file(data.join(SETTINGS_FILE)).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", data.join(SETTINGS_FILE)).unwrap();
        let (engine, _, lines) = start_logged_engine(dir.path());
        engine.stop();
        let lines = lines.lock().unwrap().clone();
        assert!(
            lines.iter().any(|l| l.starts_with("sukkula: WARN ")
                && l.contains("settings file unreadable; every protocol is off")
                && l.contains(r#"why="not a regular file of ours""#)),
            "{lines:?}"
        );
    }

    fn settings_events(log: &Arc<Mutex<Vec<Event>>>) -> Vec<(Settings, bool)> {
        log.lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                Event::Settings {
                    settings,
                    recovered,
                    ..
                } => Some((settings.clone(), *recovered)),
                _ => None,
            })
            .collect()
    }

    /// The review's case: a mailbox URL an earlier build took costs the
    /// wormhole section, not the PIN and Hidden; the UI is told until the
    /// user saves; the file is not touched meanwhile.
    #[test]
    fn a_settings_file_that_does_not_read_fails_closed_and_the_ui_is_told() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        Store::open(&data).unwrap();
        let saved = r#"{"localsend":{"enabled":true,"pin":"4711"},"quickshare":{"enabled":true,"visibility":"hidden","ble_nudge":false},"wormhole":{"mailbox_url":"wss://relay.example/v1?token=x"}}"#;
        std::fs::write(data.join(SETTINGS_FILE), saved).unwrap();
        let (engine, log) = start_engine(dir.path());
        let (s, recovered) = settings_events(&log)[0].clone();
        assert!(recovered);
        assert_eq!(s.localsend.pin.as_deref(), Some("4711"));
        assert!(s.localsend.enabled);
        assert_eq!(
            s.quickshare.visibility,
            sukkula_core::config::Visibility::Hidden
        );
        assert!(!s.wormhole.enabled);
        assert_eq!(s.wormhole.mailbox_url, None);
        engine.command_json(r#"{"v":1,"id":1,"cmd":{"type":"get_settings"}}"#);
        wait_for_reply(&log, 1);
        assert!(settings_events(&log)[1].1, "told again until saved");
        assert_eq!(
            std::fs::read_to_string(data.join(SETTINGS_FILE)).unwrap(),
            saved,
            "the file is left as it is"
        );
        // Receive with a disabled wormhole is refused.
        engine.command_json(
            r#"{"v":1,"id":2,"cmd":{"type":"receive_wormhole","code":"7-guitarist-revenge"}}"#,
        );
        assert!(matches!(
            wait_for_reply(&log, 2),
            Event::Reply { ok: false, error: Some(e), .. } if e.code == ErrorCode::Unavailable
        ));
        // Saving settings replaces the file, and the flag goes.
        let mut fixed = s.clone();
        fixed.wormhole.enabled = true;
        let cmd = serde_json::json!({"v":1,"id":3,"cmd":{"type":"set_settings","settings":fixed}});
        engine.command_json(&cmd.to_string());
        assert!(matches!(
            wait_for_reply(&log, 3),
            Event::Reply { ok: true, .. }
        ));
        let (after, recovered) = settings_events(&log).last().unwrap().clone();
        assert!(!recovered);
        assert_eq!(after, fixed);
        engine.stop();
        let (engine, log) = start_engine(dir.path());
        assert_eq!(settings_events(&log)[0], (fixed, false));
        engine.stop();
    }

    fn progress(n: u64) -> Event {
        Event::TransferProgress {
            transfer: 1,
            bytes: n,
            total: u64::MAX,
        }
    }

    fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn start_emits_started_settings_receiving() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, log) = start_engine(dir.path());
        {
            let log = log.lock().unwrap();
            assert_eq!(log.len(), 3, "{log:?}");
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

    #[test]
    fn a_panicking_command_is_still_answered_once() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (delivery, log) = recording_delivery();
        let slots = Arc::new(AtomicUsize::new(0));
        let guard = |id| {
            ReplyGuard::new(
                delivery.gate.clone(),
                id,
                Permit::acquire(&slots).unwrap(),
                CancellationToken::new(),
            )
        };
        let limit = Some(COMMAND_TIMEOUT);
        spawn_reply(runtime.handle(), guard(1), limit, async {
            panic!("a bug in dispatch");
        });
        spawn_reply(runtime.handle(), guard(2), limit, async { Ok(Some(9)) });
        spawn_reply(runtime.handle(), guard(3), None, std::future::pending());
        wait_until("two replies", || log.lock().unwrap().len() == 2);
        // The runtime drops the pending task, and its guard answers it.
        drop(runtime);
        wait_until("three replies", || log.lock().unwrap().len() == 3);
        std::thread::sleep(Duration::from_millis(20));
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 3);
        for id in 1..=3 {
            let replies: Vec<_> = log
                .iter()
                .filter(|e| matches!(e, Event::Reply { id: i, .. } if *i == id))
                .collect();
            assert_eq!(replies.len(), 1, "id {id}: {log:?}");
            let expect_ok = id == 2;
            assert!(
                matches!(replies[0], Event::Reply { ok, .. } if *ok == expect_ok),
                "{replies:?}"
            );
        }
        assert_eq!(slots.load(Ordering::Acquire), 0, "every slot came back");
    }

    #[test]
    fn slots_are_bounded_and_come_back() {
        let slots = Arc::new(AtomicUsize::new(0));
        let held: Vec<_> = (0..MAX_IN_FLIGHT_COMMANDS)
            .map(|_| Permit::acquire(&slots).unwrap())
            .collect();
        assert!(Permit::acquire(&slots).is_none());
        drop(held);
        assert_eq!(slots.load(Ordering::Acquire), 0);
        assert!(Permit::acquire(&slots).is_some());
    }

    #[test]
    fn a_flood_of_commands_is_refused_not_queued() {
        let dir = tempfile::tempdir().unwrap();
        // A sink that holds every event until told to go on: no reply is
        // delivered, so no slot comes back.
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = Mutex::new(go_rx);
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        let started = Arc::new(AtomicUsize::new(0));
        let s = started.clone();
        let sink: EventSink = Arc::new(move |e: Event| {
            if s.fetch_add(1, Ordering::SeqCst) >= 3 {
                let _ = go_rx.lock().unwrap().recv();
            }
            l.lock().unwrap().push(e);
        });
        let engine = Engine::start(config(dir.path()), sink).unwrap();
        let mut taken = 0;
        let mut refused = 0;
        for id in 0..(MAX_IN_FLIGHT_COMMANDS as u64 * 4) {
            match engine.try_command_json(&format!(
                r#"{{"v":1,"id":{id},"cmd":{{"type":"answer","offer":1,"accept":false}}}}"#
            )) {
                Ok(()) => taken += 1,
                Err(Refused::Busy) => refused += 1,
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(taken, MAX_IN_FLIGHT_COMMANDS);
        assert_eq!(refused, MAX_IN_FLIGHT_COMMANDS * 3);
        for _ in 0..taken {
            go_tx.send(()).unwrap();
        }
        wait_until("every reply", || log.lock().unwrap().len() == 3 + taken);
        wait_until("every slot", || {
            engine.hub.in_flight.load(Ordering::Acquire) == 0
        });
        // Room again.
        engine
            .try_command_json(r#"{"v":1,"id":999,"cmd":{"type":"get_settings"}}"#)
            .unwrap();
        drop(go_tx);
        engine.stop();
    }

    #[test]
    fn the_queue_is_bounded_and_waits_for_room() {
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = Mutex::new(go_rx);
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        let sink: EventSink = Arc::new(move |e: Event| {
            let _ = go_rx.lock().unwrap().recv();
            l.lock().unwrap().push(e);
        });
        let delivery = Delivery::spawn(sink, &quiet()).unwrap();
        let gate = delivery.gate.clone();
        let total = MAX_QUEUED_EVENTS + 100;
        let pushed = Arc::new(AtomicUsize::new(0));
        let p = pushed.clone();
        let g = gate.clone();
        let emitter = std::thread::spawn(move || {
            for n in 0..total {
                g.emit(progress(n as u64));
                p.fetch_add(1, Ordering::SeqCst);
            }
        });
        // One is in the sink, MAX_QUEUED_EVENTS wait, and the emitter is
        // stuck on the next.
        wait_until("a full queue", || {
            pushed.load(Ordering::SeqCst) == MAX_QUEUED_EVENTS + 1
        });
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(pushed.load(Ordering::SeqCst), MAX_QUEUED_EVENTS + 1);
        assert!(gate.lock().held_events <= MAX_QUEUED_EVENTS);
        // A reply holding a slot does not wait.
        let slots = Arc::new(AtomicUsize::new(0));
        assert!(
            gate.push(reply_event(1, Ok(None)), Permit::acquire(&slots))
                .is_some()
        );
        for _ in 0..=total {
            go_tx.send(()).unwrap();
        }
        emitter.join().unwrap();
        wait_until("everything", || log.lock().unwrap().len() == total + 1);
        // In order, the reply after the events queued before it.
        let log = log.lock().unwrap();
        let order: Vec<u64> = log
            .iter()
            .filter_map(|e| match e {
                Event::TransferProgress { bytes, .. } => Some(*bytes),
                _ => None,
            })
            .collect();
        assert_eq!(order, (0..total as u64).collect::<Vec<_>>());
        drop(log);
        drop(delivery);
        assert_eq!(slots.load(Ordering::Acquire), 0);
    }

    #[test]
    fn the_queue_is_bounded_in_bytes_too() {
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = Mutex::new(go_rx);
        let sink: EventSink = Arc::new(move |_: Event| {
            let _ = go_rx.lock().unwrap().recv();
        });
        let delivery = Delivery::spawn(sink, &quiet()).unwrap();
        let gate = delivery.gate.clone();
        // About 200 KiB each: the byte bound binds long before the count.
        let big = || Event::PeerLost {
            peer: "x".repeat(200 * 1024),
        };
        let pushed = Arc::new(AtomicUsize::new(0));
        let p = pushed.clone();
        let g = gate.clone();
        let emitter = std::thread::spawn(move || {
            for _ in 0..40 {
                g.emit(big());
                p.fetch_add(1, Ordering::SeqCst);
            }
        });
        wait_until("a full queue", || pushed.load(Ordering::SeqCst) >= 10);
        std::thread::sleep(Duration::from_millis(50));
        let n = pushed.load(Ordering::SeqCst);
        assert!(n < 40, "{n}");
        assert!(gate.lock().held_bytes <= MAX_QUEUED_BYTES);
        drop(go_tx);
        emitter.join().unwrap();
        drop(delivery);
    }

    #[test]
    fn the_sink_may_emit_without_deadlocking() {
        let slot: Arc<Mutex<Option<Arc<Gate>>>> = Arc::new(Mutex::new(None));
        let s = slot.clone();
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        let sink: EventSink = Arc::new(move |e: Event| {
            if let Event::PeerLost { peer } = &e
                && peer == "echo"
            {
                let gate = s.lock().unwrap().clone().unwrap();
                // Far more than fit: the delivery thread never waits.
                for n in 0..(MAX_QUEUED_EVENTS as u64 * 2) {
                    gate.emit(progress(n));
                }
            }
            l.lock().unwrap().push(e);
        });
        let delivery = Delivery::spawn(sink, &quiet()).unwrap();
        *slot.lock().unwrap() = Some(delivery.gate.clone());
        delivery.gate.emit(Event::PeerLost {
            peer: "echo".into(),
        });
        wait_until("every echo", || {
            log.lock().unwrap().len() == 1 + MAX_QUEUED_EVENTS * 2
        });
        slot.lock().unwrap().take();
        drop(delivery);
    }

    #[test]
    fn oversized_events_are_replaced_or_dropped() {
        let (delivery, log) = recording_delivery();
        let huge = "x".repeat(MAX_EVENT_BYTES);
        let slots = Arc::new(AtomicUsize::new(0));
        delivery.gate.push(
            Event::Reply {
                id: 7,
                ok: false,
                error: Some(ErrorInfo {
                    code: ErrorCode::Internal,
                    message: huge.clone(),
                }),
                transfer: None,
            },
            Permit::acquire(&slots),
        );
        delivery.gate.emit(Event::TransferFinished {
            transfer: 3,
            outcome: Outcome::Done,
            saved: vec![huge.clone()],
        });
        delivery.gate.emit(Event::PeerLost { peer: huge });
        delivery.gate.emit(Event::OfferClosed {
            offer: 1,
            reason: Closed::Declined,
        });
        wait_until("three events", || log.lock().unwrap().len() == 3);
        std::thread::sleep(Duration::from_millis(20));
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 3, "the peer event was dropped");
        assert!(
            matches!(&log[0], Event::Reply { id: 7, ok: false, error: Some(e), .. } if e.code == ErrorCode::Internal && e.message.len() < 100)
        );
        assert!(matches!(
            &log[1],
            Event::TransferFinished {
                transfer: 3,
                outcome: Outcome::Failed { .. },
                saved,
            } if saved.is_empty()
        ));
        assert!(matches!(log[2], Event::OfferClosed { offer: 1, .. }));
        for e in log.iter() {
            assert!(encoded_len(e).is_some());
        }
    }

    #[test]
    fn stop_from_inside_the_sink_does_not_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let slot: Arc<Mutex<Option<Engine>>> = Arc::new(Mutex::new(None));
        let s = slot.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let done_tx = Mutex::new(done_tx);
        let sink: EventSink = Arc::new(move |e: Event| {
            c.fetch_add(1, Ordering::SeqCst);
            if matches!(e, Event::Reply { id: 1, .. }) {
                let engine = s.lock().unwrap().take().unwrap();
                engine.stop();
                done_tx.lock().unwrap().send(()).unwrap();
            }
        });
        let engine = Engine::start(config(dir.path()), sink).unwrap();
        {
            let mut held = slot.lock().unwrap();
            let engine = held.insert(engine);
            engine.command_json(r#"{"v":1,"id":1,"cmd":{"type":"get_settings"}}"#);
        }
        done_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        let n = calls.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(calls.load(Ordering::SeqCst), n, "called after stop");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_and_stop_inside_another_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, log) = start_engine(dir.path());
        engine.command_json(r#"{"v":1,"id":1,"cmd":{"type":"get_settings"}}"#);
        tokio::time::sleep(Duration::from_millis(50)).await;
        engine.stop();
        let n = log.lock().unwrap().len();
        assert!(n >= 3);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(log.lock().unwrap().len(), n);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drop_inside_a_current_thread_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, _log) = start_engine(dir.path());
        drop(engine);
    }

    #[test]
    fn config_paths_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_string_lossy().into_owned();
        let sink: EventSink = Arc::new(|_: Event| panic!("no event for a failed start"));
        let cases = [
            ("relative/data".to_owned(), format!("{base}/dl")),
            (format!("{base}/data"), format!("{base}/data")),
            (format!("{base}/data"), format!("{base}/data/dl")),
            (format!("{base}/dl/data"), format!("{base}/dl")),
            (format!("{base}/a/../data"), format!("{base}/dl")),
            (format!("{base}/./data"), format!("{base}/dl")),
            (format!("{base}/da\nta"), format!("{base}/dl")),
            (format!("{base}/data\0"), format!("{base}/dl")),
            ("/".to_owned(), format!("{base}/dl")),
            (
                format!("/{}", "a".repeat(MAX_PATH_BYTES)),
                format!("{base}/dl"),
            ),
        ];
        for (data, dl) in cases {
            let cfg = StartConfig {
                v: API_VERSION,
                data_dir: data.clone(),
                download_dir: dl.clone(),
                device_model: None,
                allow_loopback: false,
            };
            match Engine::start(cfg, sink.clone()) {
                Err(e) => assert_eq!(e.code, ErrorCode::BadCommand, "{data} {dl}"),
                Ok(_) => panic!("{data} {dl} was accepted"),
            }
        }
        // Different strings, one directory.
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
        let cfg = StartConfig {
            v: API_VERSION,
            data_dir: format!("{base}/link/data"),
            download_dir: format!("{base}/real"),
            device_model: None,
            allow_loopback: false,
        };
        assert_eq!(
            Engine::start(cfg, sink.clone()).err().map(|e| e.code),
            Some(ErrorCode::BadCommand)
        );
        let cfg = StartConfig {
            v: 2,
            ..config(dir.path())
        };
        assert_eq!(
            Engine::start(cfg, sink).err().map(|e| e.code),
            Some(ErrorCode::BadVersion)
        );
    }

    #[test]
    fn hw_release_names_the_model() {
        assert_eq!(
            parse_hw_release(
                "# comment\nID=x\nNAME=\"Jolla Phone\"\nMER_HA_DEVICE=jp2026\nVERSION_ID=5.2\n"
            ),
            "Jolla Phone"
        );
        assert_eq!(parse_hw_release("MER_HA_DEVICE=jp2026\n"), "jp2026");
        assert_eq!(
            parse_hw_release("NAME=\"\u{202E}\"\nMER_HA_DEVICE='jp2026'\n"),
            "jp2026",
            "an empty name after S2 falls through"
        );
        assert_eq!(parse_hw_release("NAME='It''s'\n"), "It''s");
        assert_eq!(parse_hw_release("NAME=\"A \\\"B\\\" C\"\n"), "A \"B\" C");
        assert_eq!(parse_hw_release("NAME=Plain Name  \n"), "Plain Name");
        assert_eq!(parse_hw_release("NAME=first\nNAME=second\n"), "first");
        assert_eq!(parse_hw_release(""), "");
        assert_eq!(parse_hw_release("garbage\n=\n==\n"), "");
        let long = parse_hw_release(&format!("NAME={}", "x".repeat(1000)));
        assert_eq!(long.chars().count(), MAX_MODEL_CHARS);
        assert_eq!(
            parse_hw_release("NAME=\"Evil\u{1b}[31m\u{202E}Phone\"\n"),
            "Evil[31mPhone"
        );
    }

    #[test]
    fn hw_release_reads_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hw-release");
        let mut content = "#".repeat(usize::try_from(MAX_HW_RELEASE_BYTES).unwrap());
        content.push_str("\nNAME=Beyond the cap\n");
        std::fs::write(&path, &content).unwrap();
        assert_eq!(read_hw_model_from(&path), "");
        std::fs::write(&path, b"NAME=Jolla \xff Phone\n").unwrap();
        assert_eq!(read_hw_model_from(&path), "Jolla \u{FFFD} Phone");
        assert_eq!(read_hw_model_from(&dir.path().join("missing")), "");
        assert_eq!(read_hw_model_from(dir.path()), "", "a directory");
        let fifo = dir.path().join("fifo");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        let started = Instant::now();
        assert_eq!(read_hw_model_from(&fifo), "", "a FIFO is not opened");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// An adapter whose every step takes as long as the test says, on the
    /// runtime's clock: start and stop `switch`, a receive by code the
    /// number of seconds the code names.
    struct Slow {
        protocol: Protocol,
        switch: Duration,
    }

    impl Adapter for Slow {
        fn protocol(&self) -> Protocol {
            self.protocol
        }

        fn start_receiving(&self) -> crate::adapter::BoxFuture<'_, Result<(), ErrorInfo>> {
            Box::pin(async move {
                tokio::time::sleep(self.switch).await;
                Ok(())
            })
        }

        fn stop_receiving(&self) -> crate::adapter::BoxFuture<'_, ()> {
            Box::pin(tokio::time::sleep(self.switch))
        }

        fn send(
            &self,
            _: SendTarget,
            _: Vec<Outgoing>,
        ) -> crate::adapter::BoxFuture<'_, Result<TransferId, ErrorInfo>> {
            Box::pin(async { Err(crate::adapter::unavailable("sending")) })
        }

        fn receive_code(
            &self,
            code: String,
        ) -> crate::adapter::BoxFuture<'_, Result<TransferId, ErrorInfo>> {
            Box::pin(async move {
                let secs: u64 = code.parse().unwrap();
                tokio::time::sleep(Duration::from_secs(secs)).await;
                Ok(7)
            })
        }
    }

    /// A hub as `Engine::start` builds one, with `adapters`, whose commands
    /// run on `runtime` -- a runtime with a paused clock, so that minutes of
    /// timeouts take no time -- and a log of what it delivers.
    fn test_hub(
        dir: &Path,
        adapters: Vec<Arc<dyn Adapter>>,
    ) -> (Arc<Hub>, Delivery, Arc<Mutex<Vec<Event>>>) {
        let (delivery, log) = recording_delivery();
        let events: EventSink = {
            let gate = delivery.gate.clone();
            Arc::new(move |e| gate.emit(e))
        };
        let ctx = Arc::new(Ctx::new(
            Settings::default(),
            "Test Phone".into(),
            Store::open(&dir.join("data")).unwrap(),
            Inbox::open(&dir.join("dl")).unwrap(),
            ConsentBroker::new(consent_observer(events.clone())),
            ReachPolicy {
                allow_loopback: true,
            },
            events,
        ));
        let statuses = statuses(&adapters, &ctx.settings(), false);
        let hub = Arc::new(Hub {
            ctx,
            adapters,
            gate: delivery.gate.clone(),
            in_flight: Arc::new(AtomicUsize::new(0)),
            state: AsyncMutex::new(HubState {
                receiving: false,
                statuses,
            }),
            discovery: AsyncMutex::new(()),
            logging: quiet(),
            recovered: AtomicBool::new(false),
        });
        (hub, delivery, log)
    }

    fn paused_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap()
    }

    fn take(hub: &Arc<Hub>, runtime: &tokio::runtime::Runtime, id: RequestId, cmd: &str) {
        let env = parse_command(&format!(r#"{{"v":1,"id":{id},"cmd":{cmd}}}"#)).unwrap();
        hub.spawn_command(runtime, env, Permit::acquire(&hub.in_flight).unwrap());
    }

    fn reply_to(
        log: &Arc<Mutex<Vec<Event>>>,
        id: RequestId,
    ) -> Result<Option<TransferId>, ErrorInfo> {
        match wait_for_reply(log, id) {
            Event::Reply {
                ok: true, transfer, ..
            } => Ok(transfer),
            Event::Reply { error: Some(e), .. } => Err(e),
            other => panic!("{other:?}"),
        }
    }

    /// A `set_receiving` that waited 56 s for the switch behind slow
    /// protocols still gets it and runs to the end: the final `receiving`
    /// comes, with no protocol left `starting`. One that would wait past
    /// 60 s is refused having changed nothing. Under a timeout around the
    /// whole command, the first was cut off half-way at 60 s, after its
    /// `starting`.
    #[test]
    fn a_switch_that_waited_long_still_finishes_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = paused_runtime();
        let slow = ADAPTER_TIMEOUT.saturating_sub(Duration::from_secs(1));
        let adapters: Vec<Arc<dyn Adapter>> = vec![
            Arc::new(Slow {
                protocol: Protocol::LocalSend,
                switch: slow,
            }),
            Arc::new(Slow {
                protocol: Protocol::QuickShare,
                switch: slow,
            }),
        ];
        let (hub, delivery, log) = test_hub(dir.path(), adapters);
        let on = r#"{"type":"set_receiving","on":true}"#;
        let off = r#"{"type":"set_receiving","on":false}"#;
        // 0-14 s: on. 14-42 s: settings, a stop and a start. 42-56 s: off.
        // 56-70 s: on, which waited 56 s. And one more, which would wait 70.
        take(&hub, &runtime, 1, on);
        take(
            &hub,
            &runtime,
            2,
            r#"{"type":"set_settings","settings":{"device_name":"x"}}"#,
        );
        take(&hub, &runtime, 3, off);
        take(&hub, &runtime, 4, on);
        take(&hub, &runtime, 5, off);
        runtime.block_on(async { tokio::time::sleep(Duration::from_secs(300)).await });
        for id in 1..=4 {
            assert_eq!(reply_to(&log, id), Ok(None), "command {id}");
        }
        let refused = reply_to(&log, 5).unwrap_err();
        assert_eq!(refused.code, ErrorCode::Internal);
        assert!(
            refused.message.contains("nothing was changed"),
            "{refused:?}"
        );
        let last = log
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|e| match e {
                Event::Receiving { on, protocols } => Some((*on, protocols.clone())),
                _ => None,
            })
            .unwrap();
        assert!(last.0, "{last:?}");
        assert!(
            last.1.iter().all(|p| p.state == ProtocolState::Ready),
            "{last:?}"
        );
        assert!(runtime.block_on(hub.state.lock()).receiving);
        assert_eq!(hub.in_flight.load(Ordering::Acquire), 0);
        drop(delivery);
    }

    /// A receive by code is answered once the user has answered (F-MW2), so
    /// it may take the mailbox connection, the key exchange and the offer
    /// at their slowest, and then the whole minute the dialog counts down,
    /// less a moment. Under the 60 s every other command has, it died
    /// while the dialog still showed time left. Bounded all the same.
    #[test]
    fn receive_wormhole_may_wait_for_the_user_as_long_as_the_offer_does() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = paused_runtime();
        let adapters: Vec<Arc<dyn Adapter>> = vec![Arc::new(Slow {
            protocol: Protocol::Wormhole,
            switch: Duration::ZERO,
        })];
        let (hub, delivery, log) = test_hub(dir.path(), adapters);
        let slowest = HANDSHAKE_TIMEOUT
            .saturating_mul(2)
            .saturating_add(NETWORK_IDLE_TIMEOUT)
            .saturating_add(OFFER_TIMEOUT)
            .saturating_sub(Duration::from_secs(2));
        let receive = |secs: u64| format!(r#"{{"type":"receive_wormhole","code":"{secs}"}}"#);
        take(&hub, &runtime, 1, &receive(slowest.as_secs()));
        take(&hub, &runtime, 2, &receive(3600));
        runtime.block_on(async { tokio::time::sleep(Duration::from_secs(400)).await });
        assert_eq!(reply_to(&log, 1), Ok(Some(7)));
        let gave_up = reply_to(&log, 2).unwrap_err();
        assert_eq!(gave_up.code, ErrorCode::Internal);
        assert!(gave_up.message.contains("in time"), "{gave_up:?}");
        // What docs/FFI.md tells the shell.
        assert_eq!(RECEIVE_CODE_TIMEOUT, Duration::from_secs(150));
        let doc = include_str!("../../../docs/FFI.md");
        assert!(doc.contains("| One command's work | 60 s; `receive_wormhole` 150 s,"));
        assert!(doc.contains("may take up to 150 s"));
        assert_eq!(COMMAND_TIMEOUT, Duration::from_secs(60));
        assert!(RECEIVE_CODE_TIMEOUT > slowest);
        drop(delivery);
    }

    #[test]
    fn every_command_has_its_time_limit() {
        let cmd = |body: &str| {
            parse_command(&format!(r#"{{"v":1,"id":1,"cmd":{body}}}"#))
                .unwrap()
                .cmd
        };
        assert_eq!(
            command_timeout(&cmd(r#"{"type":"set_receiving","on":true}"#)),
            None
        );
        assert_eq!(
            command_timeout(&cmd(r#"{"type":"set_settings","settings":{}}"#)),
            None
        );
        assert_eq!(
            command_timeout(&cmd(r#"{"type":"receive_wormhole","code":"7-a-b"}"#)),
            Some(RECEIVE_CODE_TIMEOUT)
        );
        for body in [
            r#"{"type":"get_settings"}"#,
            r#"{"type":"start_discovery"}"#,
            r#"{"type":"answer","offer":1,"accept":true}"#,
            r#"{"type":"cancel","transfer":1}"#,
            r#"{"type":"list_bluetooth_devices"}"#,
        ] {
            assert_eq!(command_timeout(&cmd(body)), Some(COMMAND_TIMEOUT), "{body}");
        }
    }

    /// What the fake adapters were asked to do, by protocol.
    type Calls = Arc<Mutex<Vec<(Protocol, &'static str)>>>;

    /// An adapter that does at once whatever it is asked, and records it.
    /// Stops are not recorded: the hub may stop anything, on or off.
    struct Fake {
        protocol: Protocol,
        calls: Calls,
    }

    impl Fake {
        fn called(&self, what: &'static str) {
            self.calls.lock().unwrap().push((self.protocol, what));
        }
    }

    impl Adapter for Fake {
        fn protocol(&self) -> Protocol {
            self.protocol
        }

        fn receives(&self) -> bool {
            matches!(self.protocol, Protocol::LocalSend | Protocol::QuickShare)
        }

        fn start_receiving(&self) -> crate::adapter::BoxFuture<'_, Result<(), ErrorInfo>> {
            self.called("start_receiving");
            Box::pin(async { Ok(()) })
        }

        fn stop_receiving(&self) -> crate::adapter::BoxFuture<'_, ()> {
            Box::pin(async {})
        }

        fn start_discovery(&self) -> crate::adapter::BoxFuture<'_, Result<(), ErrorInfo>> {
            self.called("start_discovery");
            Box::pin(async { Ok(()) })
        }

        fn send(
            &self,
            _: SendTarget,
            _: Vec<Outgoing>,
        ) -> crate::adapter::BoxFuture<'_, Result<TransferId, ErrorInfo>> {
            self.called("send");
            Box::pin(async { Ok(1) })
        }

        fn receive_code(
            &self,
            _: String,
        ) -> crate::adapter::BoxFuture<'_, Result<TransferId, ErrorInfo>> {
            self.called("receive_code");
            Box::pin(async { Ok(2) })
        }

        fn list_devices(
            &self,
        ) -> crate::adapter::BoxFuture<'_, Result<Vec<crate::api::BluetoothDevice>, ErrorInfo>>
        {
            self.called("list_devices");
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    const EVERY_PROTOCOL: [Protocol; 4] = [
        Protocol::LocalSend,
        Protocol::QuickShare,
        Protocol::Wormhole,
        Protocol::Bluetooth,
    ];

    fn switch_off(settings: &mut Settings, protocol: Protocol) {
        match protocol {
            Protocol::LocalSend => settings.localsend.enabled = false,
            Protocol::QuickShare => settings.quickshare.enabled = false,
            Protocol::Wormhole => settings.wormhole.enabled = false,
            Protocol::Bluetooth => settings.bluetooth.enabled = false,
        }
    }

    /// The commands that use `protocol`, with what each asks of its adapter.
    fn commands_of(protocol: Protocol) -> Vec<(String, &'static str)> {
        let send = |target: &str| {
            format!(
                r#"{{"type":"send","target":{target},"items":[{{"kind":"text","text":"hi"}}]}}"#
            )
        };
        match protocol {
            Protocol::LocalSend => {
                vec![(send(r#"{"protocol":"local_send","peer":"ls-1"}"#), "send")]
            }
            Protocol::QuickShare => {
                vec![(send(r#"{"protocol":"quick_share","peer":"qs-1"}"#), "send")]
            }
            Protocol::Wormhole => vec![
                (send(r#"{"protocol":"wormhole"}"#), "send"),
                (
                    r#"{"type":"receive_wormhole","code":"7-guitarist-revenge"}"#.to_owned(),
                    "receive_code",
                ),
            ],
            Protocol::Bluetooth => vec![
                (
                    send(r#"{"protocol":"bluetooth","address":"AA:BB:CC:DD:EE:FF"}"#),
                    "send",
                ),
                (
                    r#"{"type":"list_bluetooth_devices"}"#.to_owned(),
                    "list_devices",
                ),
            ],
        }
    }

    /// Whether `protocol` receives: the others only send.
    fn receives(protocol: Protocol) -> bool {
        matches!(protocol, Protocol::LocalSend | Protocol::QuickShare)
    }

    /// The state the last `receiving` event gave `protocol`.
    fn final_state(log: &Arc<Mutex<Vec<Event>>>, protocol: Protocol) -> ProtocolState {
        log.lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|e| match e {
                Event::Receiving { protocols, .. } => protocols
                    .iter()
                    .find(|s| s.protocol == protocol)
                    .map(|s| s.state),
                _ => None,
            })
            .unwrap()
    }

    /// F-C1: each protocol can be switched off in the settings, and then
    /// nothing uses it. Receive switched on leaves it off, discovery does
    /// not start it, and its commands -- sends, a receive by code, the
    /// device list -- are `unavailable` without reaching it. Switched off
    /// while receiving, it stops. Every other protocol meanwhile does all
    /// of that, so the test cannot pass by nothing working. Wormhole could
    /// not be switched off at all ("Magic Wormhole cannot be disabled in
    /// Settings").
    #[test]
    fn a_protocol_switched_off_in_the_settings_is_used_by_no_command() {
        for off in EVERY_PROTOCOL {
            switched_off_is_used_by_no_command(off);
        }
    }

    /// One round of the test above: `off` switched off, every other
    /// protocol on.
    fn switched_off_is_used_by_no_command(off: Protocol) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = paused_runtime();
        let calls = Calls::default();
        let adapters: Vec<Arc<dyn Adapter>> = EVERY_PROTOCOL
            .iter()
            .map(|&protocol| {
                Arc::new(Fake {
                    protocol,
                    calls: calls.clone(),
                }) as Arc<dyn Adapter>
            })
            .collect();
        let (hub, delivery, log) = test_hub(dir.path(), adapters);
        let mut next = 0;
        let mut run = |cmd: &str| {
            next += 1;
            take(&hub, &runtime, next, cmd);
            runtime.block_on(async { tokio::time::sleep(Duration::from_secs(1)).await });
            reply_to(&log, next)
        };
        let mut settings = Settings::default();
        switch_off(&mut settings, off);
        let set = serde_json::json!({"type": "set_settings", "settings": settings});
        run(&set.to_string()).unwrap();
        run(r#"{"type":"set_receiving","on":true}"#).unwrap();
        run(r#"{"type":"start_discovery"}"#).unwrap();
        for p in EVERY_PROTOCOL {
            for (cmd, _) in commands_of(p) {
                let reply = run(&cmd);
                if p == off {
                    assert_eq!(
                        reply.unwrap_err().code,
                        ErrorCode::Unavailable,
                        "{off:?} off: {cmd}"
                    );
                } else {
                    assert!(reply.is_ok(), "{off:?} off: {cmd}: {reply:?}");
                }
            }
        }
        let asked = calls.lock().unwrap().clone();
        assert!(
            asked.iter().all(|(p, _)| *p != off),
            "{off:?} is off, yet: {asked:?}"
        );
        for p in EVERY_PROTOCOL.into_iter().filter(|p| *p != off) {
            let mut expected = vec!["start_discovery"];
            if receives(p) {
                expected.push("start_receiving");
            }
            expected.extend(commands_of(p).into_iter().map(|(_, call)| call));
            for call in expected {
                assert!(asked.contains(&(p, call)), "{p:?} never asked {call}");
            }
        }
        for p in EVERY_PROTOCOL {
            let expected = match (receives(p), p == off) {
                (false, _) => ProtocolState::SendOnly,
                (true, true) => ProtocolState::Off,
                (true, false) => ProtocolState::Ready,
            };
            assert_eq!(final_state(&log, p), expected, "{off:?} off: {p:?}");
        }

        // Everything on while receiving, then `off` switched off again:
        // it stops, and its commands go with it.
        run(r#"{"type":"set_settings","settings":{}}"#).unwrap();
        for p in EVERY_PROTOCOL.into_iter().filter(|p| receives(*p)) {
            assert_eq!(final_state(&log, p), ProtocolState::Ready, "{p:?}");
        }
        run(&set.to_string()).unwrap();
        if receives(off) {
            assert_eq!(final_state(&log, off), ProtocolState::Off, "{off:?}");
        }
        for (cmd, _) in commands_of(off) {
            assert_eq!(run(&cmd).unwrap_err().code, ErrorCode::Unavailable, "{cmd}");
        }
        drop(delivery);
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
