//! The engine's log (S9): off by default, to standard error only, and never
//! with the user's or a peer's data at info level or above.
//!
//! # Where it goes
//!
//! One line per event on standard error, which on Sailfish is where an
//! app's output goes: the systemd journal (`journalctl --user`). Never a
//! file: only `sukkula_core` writes files (S3), and a log file would be one
//! more place a name could outlive its transfer. The journal adds the time
//! and the process; a line is
//!
//! ```text
//! sukkula: WARN sukkula_engine::hub: settings unreadable; using defaults why="malformed"
//! ```
//!
//! Every character S2 would not show -- line breaks, other controls, bidi
//! and invisible characters -- is written as a `\u{..}` escape, so no value
//! can forge a line of its own or reorder one on a terminal, and a line is
//! cut at [`MAX_LINE_BYTES`], below `PIPE_BUF`, so it reaches the journal in
//! one write.
//!
//! # What it says
//!
//! | Target | Logging off (default) | Logging on |
//! | --- | --- | --- |
//! | `sukkula_core`, `sukkula_engine`, `sukkula_ffi` | warn | debug |
//! | `rqs_lib` (Quick Share) | nothing | info |
//! | `magic_wormhole` | nothing | info |
//! | `magic_wormhole::core` | nothing | nothing |
//! | `localsend` | nothing | error |
//! | anything else | nothing | warn |
//!
//! Sukkula's own crates log sizes, counts, protocols, error codes and fixed
//! reasons, never a name, a text, an alias, a PIN, a code, a key, a path in
//! the download directory or a peer's address, at any level:
//! `tests/s9_logs.rs` runs transfers with distinctive values and looks for
//! them. Dependencies are capped at the level up to which their messages
//! were read and found free of such values; the caps below that are
//! findings:
//!
//! - `magic_wormhole::core` logs a mailbox message of a type it does not
//!   know verbatim at warn (`core/rendezvous.rs`), which lets any mailbox
//!   server write into the journal (the guard passes unknown types on;
//!   `tests/s9_logs.rs` has a server do it), and a peer message it cannot
//!   parse, decrypted, at error (`core.rs`, `receive_json`), which could
//!   carry an offer's text or file names (Sukkula reads peer messages
//!   with `receive` and parses them itself, so this one is not reached
//!   today). Off entirely.
//! - `localsend` logs a malformed multicast announcement with the sender's
//!   address and serde's description of it, which quotes what the peer
//!   sent, at warn (`multicast/mod.rs`). Error only.
//! - `rqs_lib` is vendored with `0014-s9-logs-without-user-data.patch`,
//!   which moved names, paths, texts, the PIN and peer strings out of its
//!   info lines; its debug lines still carry peer addresses. Info.
//! - Debug and trace from dependencies are never shown: they are the
//!   libraries' own protocol traces, with addresses and frame contents.
//!
//! [`would_log`] is the table, for tests to hold other code to it.
//!
//! # Whose it is
//!
//! Each engine has a subscriber of its own (`Logging`), made the thread's
//! default on every thread the engine runs code on: the runtime's workers
//! and blocking threads (`on_thread_start`), the delivery thread, and a
//! caller's thread for the duration of `Engine::start`, `stop` and each
//! command. It is never the process's global default: the process belongs
//! to Qt, and two engines (the tests have many) must not share a switch.
//! Events on threads that are not the engine's -- async-io's reactor, the
//! mDNS daemon's thread -- go nowhere.
//!
//! The switch is a flag the filter reads on every event, not a reload of
//! tracing's per-callsite cache: what the filter tells tracing to cache
//! (which call sites can ever be shown, the most verbose level) is the same
//! with logging off and on, so it never has to be rebuilt. tracing-core
//! rebuilds that cache for every subscriber in the process at once and does
//! not serialise the rebuilds, so a switch that needed one could be undone
//! by another engine's start racing it.
//!
//! The one process-wide piece is the bridge from the `log` crate, which
//! rqs_lib, mdns-sd, tungstenite and rustls-platform-verifier use: `log`
//! has exactly one logger per process, so the first engine installs
//! [`tracing_log::LogTracer`], once. The bridge holds no policy of its own:
//! it hands each record to the calling thread's default subscriber, which
//! is an engine's (and the table above decides), or nobody. `log`'s own
//! level, also process-wide, is set to info there, so a debug or trace
//! record of a `log` user is not even formatted. If something else in the
//! process had already installed a `log` logger, it is left alone and
//! `log` records do not reach the engine's log.

use std::cell::RefCell;
use std::fmt::{self, Write as _};
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, PoisonError};

use sukkula_core::text;
use tracing::dispatcher::{self, DefaultGuard, Dispatch};
use tracing::field::{Field, Visit};
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_log::NormalizeEvent as _;
use tracing_subscriber::Registry;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};

/// Where log lines go: one call per line, the line ending in `\n`. Called
/// from engine threads; must not block for long.
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// The longest line, in bytes, the `\n` included. Below Linux's `PIPE_BUF`
/// (4096), so a line is one atomic write.
pub const MAX_LINE_BYTES: usize = 2048;

/// Sukkula's own crates.
const OURS: [&str; 3] = ["sukkula_core", "sukkula_engine", "sukkula_ffi"];

/// Most bytes one escaped character takes: `\u{10ffff}`.
const MAX_ESCAPE_BYTES: usize = 10;

/// Room kept at the end of a line for `…` (3 bytes) and `\n`.
const LINE_END_BYTES: usize = 4;

/// The sink the C ABI uses: standard error, one write per line.
#[must_use]
pub fn stderr() -> LogSink {
    Arc::new(|line: &str| {
        // A closed or full standard error loses the line; it is never the
        // engine's problem.
        let _ = std::io::stderr().write_all(line.as_bytes());
    })
}

/// Whether the engine's log shows an event of `level` from `target`, with
/// logging `on` or off: the table in the module docs.
#[must_use]
pub fn would_log(on: bool, target: &str, level: &Level) -> bool {
    policy(on).would_enable(target, level)
}

/// The table in the module docs, as a filter.
fn policy(on: bool) -> Targets {
    let ours = if on {
        LevelFilter::DEBUG
    } else {
        LevelFilter::WARN
    };
    let mine = OURS.iter().fold(Targets::new(), |t, crate_name| {
        t.with_target(*crate_name, ours)
    });
    if !on {
        // No default: everything else is off.
        return mine;
    }
    mine.with_target("rqs_lib", LevelFilter::INFO)
        .with_target("magic_wormhole", LevelFilter::INFO)
        .with_target("magic_wormhole::core", LevelFilter::OFF)
        .with_target("localsend", LevelFilter::ERROR)
        .with_default(LevelFilter::WARN)
}

/// One engine's log: its subscriber, and the switch.
#[derive(Clone)]
pub(crate) struct Logging {
    dispatch: Dispatch,
    switch: Arc<Switch>,
    /// Held across a switch, so that two never interleave their lines.
    switching: Arc<Mutex<()>>,
}

impl Logging {
    /// A log writing to `sink`, on or off.
    pub(crate) fn new(sink: LogSink, on: bool) -> Logging {
        bridge_log();
        let switch = Arc::new(Switch {
            on: AtomicBool::new(on),
            quiet: policy(false),
            loud: policy(true),
        });
        let subscriber = Registry::default()
            .with(Filter(switch.clone()))
            .with(Lines { sink });
        Logging {
            dispatch: Dispatch::new(subscriber),
            switch,
            switching: Arc::new(Mutex::new(())),
        }
    }

    /// Turns debug logging on or off (`Settings::logging`), from the next
    /// event on, on every thread of the engine.
    pub(crate) fn set(&self, on: bool) {
        let _held = self
            .switching
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.switch.on.load(Ordering::SeqCst) == on {
            return;
        }
        // Said while the level lets it through: after switching on, and
        // before switching off.
        if on {
            self.switch.on.store(true, Ordering::SeqCst);
            self.scope(|| tracing::info!("debug logging on"));
        } else {
            self.scope(|| tracing::info!("debug logging off"));
            self.switch.on.store(false, Ordering::SeqCst);
        }
    }

    /// Runs `f` with this log as the calling thread's default.
    pub(crate) fn scope<T>(&self, f: impl FnOnce() -> T) -> T {
        dispatcher::with_default(&self.dispatch, f)
    }

    /// Makes this log the calling thread's default until
    /// [`leave_thread`](Self::leave_thread) or the thread's end. For threads
    /// the engine starts.
    pub(crate) fn enter_thread(&self) {
        let _ = THREAD_LOG.try_with(|slot| {
            // The old guard goes first: dropping it after setting the new
            // one would restore what was there before the old one.
            drop(slot.borrow_mut().take());
            *slot.borrow_mut() = Some(dispatcher::set_default(&self.dispatch));
        });
    }

    /// Undoes [`enter_thread`](Self::enter_thread) on the calling thread.
    pub(crate) fn leave_thread() {
        let _ = THREAD_LOG.try_with(|slot| drop(slot.borrow_mut().take()));
    }
}

thread_local! {
    /// The engine log a thread of the engine's was given.
    static THREAD_LOG: RefCell<Option<DefaultGuard>> = const { RefCell::new(None) };
}

/// Installs the `log` bridge, once per process, with `log`'s level at
/// info: the most the table shows of any `log` user. See the module docs.
fn bridge_log() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = tracing_log::LogTracer::builder()
            .with_max_level(tracing_log::log::LevelFilter::Info)
            .init();
    });
}

/// The table, off and on, and which of the two is in force.
struct Switch {
    on: AtomicBool,
    quiet: Targets,
    loud: Targets,
}

/// The engine log's filter: [`Switch`] as a layer.
struct Filter(Arc<Switch>);

impl<S: Subscriber> Layer<S> for Filter {
    /// Asked once per call site, and cached process-wide: whether the
    /// site could be shown with logging on. Never depends on the switch.
    fn register_callsite(&self, meta: &'static Metadata<'static>) -> Interest {
        if self.0.loud.would_enable(meta.target(), meta.level()) {
            Interest::sometimes()
        } else {
            Interest::never()
        }
    }

    fn enabled(&self, meta: &Metadata<'_>, _: Context<'_, S>) -> bool {
        let table = if self.0.on.load(Ordering::Relaxed) {
            &self.0.loud
        } else {
            &self.0.quiet
        };
        table.would_enable(meta.target(), meta.level())
    }

    /// The most verbose level the table ever shows, off or on.
    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::DEBUG)
    }
}

/// Formats each event the filter let through as one line for the sink.
struct Lines {
    sink: LogSink,
}

impl<S: Subscriber> Layer<S> for Lines {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        // A `log` record arrives as an event of tracing-log's; its own
        // target and level are in the normalised metadata.
        let normalized = event.normalized_metadata();
        let meta = normalized.as_ref().unwrap_or_else(|| event.metadata());
        let mut line = Line::default();
        let _ = write!(line, "sukkula: {} {}:", meta.level(), meta.target());
        event.record(&mut Message(&mut line));
        event.record(&mut Fields(&mut line));
        (self.sink)(&line.finish());
    }
}

/// One line being built: every character S2 would not show escaped, and
/// cut with `…` at [`MAX_LINE_BYTES`]. Once full, every write fails, which
/// stops the formatting of whatever is being written.
#[derive(Default)]
struct Line {
    text: String,
    full: bool,
}

impl Line {
    fn push(&mut self, c: char) -> fmt::Result {
        if self.full {
            return Err(fmt::Error);
        }
        let escape = c != ' ' && text::is_forbidden(c);
        let need = if escape {
            MAX_ESCAPE_BYTES
        } else {
            c.len_utf8()
        };
        let room = MAX_LINE_BYTES.saturating_sub(LINE_END_BYTES);
        if self.text.len().saturating_add(need) > room {
            self.text.push('…');
            self.full = true;
            return Err(fmt::Error);
        }
        if escape {
            write!(self.text, "\\u{{{:x}}}", u32::from(c))
        } else {
            self.text.push(c);
            Ok(())
        }
    }

    fn finish(mut self) -> String {
        self.text.push('\n');
        self.text
    }
}

impl fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        s.chars().try_for_each(|c| self.push(c))
    }
}

/// Writes an event's message.
struct Message<'a>(&'a mut Line);

impl Visit for Message<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            let _ = write!(self.0, " {value}");
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, " {value:?}");
        }
    }
}

/// Writes an event's other fields as ` name=value`, strings quoted. The
/// `log.*` fields tracing-log adds (the record's module, file and line)
/// are left out.
struct Fields<'a>(&'a mut Line);

impl Fields<'_> {
    fn shown(field: &Field) -> bool {
        field.name() != "message" && !field.name().starts_with("log.")
    }
}

impl Visit for Fields<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if Self::shown(field) {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if Self::shown(field) {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    /// A log into a buffer.
    fn buffered(on: bool) -> (Logging, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let l = lines.clone();
        let sink: LogSink = Arc::new(move |line| l.lock().unwrap().push(line.to_owned()));
        (Logging::new(sink, on), lines)
    }

    fn take(lines: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        std::mem::take(&mut *lines.lock().unwrap())
    }

    /// One event of each level from `target`, through `log`.
    fn every_level(log: &Logging, target: &'static str) {
        log.scope(|| {
            // `target:` must be a literal for tracing's macros; these are
            // the targets the table names.
            macro_rules! all {
                ($t:literal) => {{
                    tracing::error!(target: $t, "e");
                    tracing::warn!(target: $t, "w");
                    tracing::info!(target: $t, "i");
                    tracing::debug!(target: $t, "d");
                    tracing::trace!(target: $t, "t");
                }};
            }
            match target {
                "sukkula_engine::hub" => all!("sukkula_engine::hub"),
                "sukkula_core::inbox" => all!("sukkula_core::inbox"),
                "rqs_lib::hdl::inbound" => all!("rqs_lib::hdl::inbound"),
                "magic_wormhole::transfer" => all!("magic_wormhole::transfer"),
                "magic_wormhole::core::rendezvous" => all!("magic_wormhole::core::rendezvous"),
                "localsend::multicast" => all!("localsend::multicast"),
                "h2::proto" => all!("h2::proto"),
                _ => panic!("no such target in the table: {target}"),
            }
        });
    }

    fn levels(lines: &[String]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.split(' ').nth(1).unwrap().to_owned())
            .collect()
    }

    #[test]
    fn off_is_warnings_of_ours_and_nothing_else() {
        let (log, lines) = buffered(false);
        for t in ["sukkula_engine::hub", "sukkula_core::inbox"] {
            every_level(&log, t);
            assert_eq!(levels(&take(&lines)), ["ERROR", "WARN"], "{t}");
        }
        for t in [
            "rqs_lib::hdl::inbound",
            "magic_wormhole::transfer",
            "magic_wormhole::core::rendezvous",
            "localsend::multicast",
            "h2::proto",
        ] {
            every_level(&log, t);
            assert!(take(&lines).is_empty(), "{t}");
        }
    }

    #[test]
    fn on_is_debug_of_ours_and_capped_dependencies() {
        let (log, lines) = buffered(true);
        let cases: [(&'static str, &[&str]); 7] = [
            ("sukkula_engine::hub", &["ERROR", "WARN", "INFO", "DEBUG"]),
            ("sukkula_core::inbox", &["ERROR", "WARN", "INFO", "DEBUG"]),
            ("rqs_lib::hdl::inbound", &["ERROR", "WARN", "INFO"]),
            ("magic_wormhole::transfer", &["ERROR", "WARN", "INFO"]),
            ("magic_wormhole::core::rendezvous", &[]),
            ("localsend::multicast", &["ERROR"]),
            ("h2::proto", &["ERROR", "WARN"]),
        ];
        for (t, want) in cases {
            every_level(&log, t);
            assert_eq!(levels(&take(&lines)), want, "{t}");
        }
    }

    #[test]
    fn the_table_is_what_the_filter_does() {
        for on in [false, true] {
            let (log, lines) = buffered(on);
            for t in [
                "sukkula_engine::hub",
                "rqs_lib::hdl::inbound",
                "magic_wormhole::core::rendezvous",
                "localsend::multicast",
                "h2::proto",
            ] {
                every_level(&log, t);
                let shown = levels(&take(&lines));
                let want: Vec<String> = [
                    Level::ERROR,
                    Level::WARN,
                    Level::INFO,
                    Level::DEBUG,
                    Level::TRACE,
                ]
                .iter()
                .filter(|l| would_log(on, t, l))
                .map(ToString::to_string)
                .collect();
                assert_eq!(shown, want, "{t}, on: {on}");
            }
        }
    }

    #[test]
    fn switching_takes_effect_at_once_and_says_so() {
        let (log, lines) = buffered(false);
        let probe = |log: &Logging| log.scope(|| tracing::debug!("probe"));
        probe(&log);
        assert!(take(&lines).is_empty());
        log.set(true);
        probe(&log);
        let got = take(&lines);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].contains("INFO") && got[0].contains("debug logging on"));
        assert!(got[1].contains("DEBUG") && got[1].contains("probe"));
        log.set(true);
        assert!(take(&lines).is_empty(), "no news, no line");
        log.set(false);
        probe(&log);
        let got = take(&lines);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("debug logging off"));
    }

    #[test]
    fn two_logs_have_two_switches() {
        let (quiet, quiet_lines) = buffered(false);
        let (loud, loud_lines) = buffered(true);
        quiet.scope(|| tracing::debug!("to the quiet one"));
        loud.scope(|| tracing::debug!("to the loud one"));
        assert!(take(&quiet_lines).is_empty());
        assert_eq!(take(&loud_lines).len(), 1);
    }

    #[test]
    fn a_thread_keeps_its_log_until_it_leaves() {
        let (log, lines) = buffered(false);
        let l = log.clone();
        std::thread::spawn(move || {
            l.enter_thread();
            tracing::warn!("inside");
            // Entering twice replaces, and does not stack.
            l.enter_thread();
            Logging::leave_thread();
            tracing::warn!("after leaving");
        })
        .join()
        .unwrap();
        let got = take(&lines);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(
            got[0],
            "sukkula: WARN sukkula_engine::logging::tests: inside\n"
        );
    }

    #[test]
    fn lines_are_escaped_and_capped() {
        let (log, lines) = buffered(true);
        log.scope(|| {
            tracing::warn!(
                count = 3,
                name = "a\nb\u{202e}c",
                "first\nsukkula: ERROR forged: line\u{1b}[31m"
            );
        });
        let got = take(&lines);
        assert_eq!(got.len(), 1);
        let line = &got[0];
        assert_eq!(line.matches('\n').count(), 1, "{line:?}");
        assert!(line.ends_with('\n'));
        assert!(!line.contains('\u{1b}') && !line.contains('\u{202e}'));
        assert!(line.starts_with("sukkula: WARN sukkula_engine::logging::tests: first\\u{a}"));
        assert!(line.contains("\\u{1b}[31m"), "{line:?}");
        // Other fields after the message; strings quoted, as Debug escapes them.
        assert!(
            line.contains(r#" count=3 name="a\nb\u{202e}c""#),
            "{line:?}"
        );

        let long = "x".repeat(10 * MAX_LINE_BYTES);
        log.scope(|| tracing::warn!(value = %long, "long"));
        let got = take(&lines);
        assert_eq!(got.len(), 1);
        assert!(got[0].len() <= MAX_LINE_BYTES, "{}", got[0].len());
        assert!(got[0].ends_with("…\n"));

        // Escapes count against the cap too.
        let controls = "\u{7}".repeat(10 * MAX_LINE_BYTES);
        log.scope(|| tracing::warn!("{controls}"));
        let got = take(&lines);
        assert!(got[0].len() <= MAX_LINE_BYTES, "{}", got[0].len());
    }

    #[test]
    fn log_records_arrive_under_their_own_target_and_cap() {
        let (log, lines) = buffered(true);
        log.scope(|| {
            tracing_log::log::info!(target: "rqs_lib::hdl::inbound", "State is now {}", 3);
            // Over the bridge's cap: never even formatted.
            tracing_log::log::debug!(target: "rqs_lib::hdl::inbound", "a frame");
            tracing_log::log::warn!(target: "mdns_sd::service_daemon", "w");
            tracing_log::log::info!(target: "mdns_sd::service_daemon", "i");
        });
        let got = take(&lines);
        // Only when this process's `log` goes to the bridge, which it does
        // unless another test installed a logger first (none does).
        assert_eq!(
            got,
            [
                "sukkula: INFO rqs_lib::hdl::inbound: State is now 3\n",
                "sukkula: WARN mdns_sd::service_daemon: w\n",
            ]
        );
    }
}
