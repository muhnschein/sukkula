//! The transit guards: every transit connection the library makes goes to
//! a guard on loopback, which makes the real connection and holds the
//! peer's side of the stream to the transit framing.
//!
//! The library reads each transit record by trusting the four-byte length
//! in front of it, `Vec::with_capacity(len)` before a byte of the record
//! has arrived, and that length is outside the encryption: the peer, the
//! relay or anyone on the path can make the library reserve 4 GiB per
//! record. It also listens on every interface for direct connections,
//! asks a hard-coded third-party STUN server for our address, and can panic
//! on what that server or a connecting stranger sends (see the module
//! docs). None of that is reachable here:
//!
//! - the library is told relay-only, so it never listens and never asks
//!   STUN, and its "relays" are these guards;
//! - a guard for one of the peer's direct hints answers the relay
//!   handshake itself and connects straight to the peer; a guard for a
//!   real relay forwards the handshake;
//! - a guard accepts only the library's connection -- loopback, first line
//!   carrying the relay token derived from this transfer's key -- and
//!   refuses any record from the peer longer than [`MAX_RECORD_WIRE_BYTES`].
//!
//! What the peer is told is unchanged: our real relay, both abilities.
//! Direct connections still happen, but only outward, from us to the
//! peer's hints.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use magic_wormhole::transit::{DirectHint, Hints, RelayHint};
use sukkula_core::reach::ReachPolicy;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::{CancellationToken, DropGuard};

use super::session::Endpoint;
use super::wire::TheirTransit;
use super::{MAX_RECORD_WIRE_BYTES, Tuning};
use crate::api::{ErrorCode, ErrorInfo};

/// Guards per transfer. The library tries the first endpoint of each of
/// our two relay hints and of the peer's two at once; one guard each keeps
/// the order ours rather than a `HashSet`'s.
pub(crate) const MAX_TARGETS: usize = 4;

/// Connection attempts one transfer makes to addresses the peer chose --
/// its direct hints and its relay -- counting those refused or timed out
/// (docs/SECURITY.md, "Wormhole peers choose where we connect"). Each
/// target the peer named gets one attempt, and at most three are kept, but
/// the count is enforced where the connections are made.
pub(crate) const MAX_PEER_CONNECTS: usize = 3;

/// The relay request line: "please relay <64 hex> for side <16 hex>\n".
const RELAY_LINE_BYTES: usize = 104;

/// Addresses tried for one relay host name.
const MAX_ADDRS_PER_HOST: usize = 4;

/// Copy buffer for the record body.
const COPY_BYTES: usize = 16 * 1024;

/// Which side of the transit handshake we are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// The sender, which picks the connection.
    Leader,
    /// The receiver.
    Follower,
}

impl Role {
    /// How many bytes of handshake the peer sends before its first record:
    /// "transit receiver <64 hex> ready\n\n" to a leader; "transit sender
    /// <64 hex> ready\n\n" and then "go\n" to a follower.
    fn peer_handshake_bytes(self) -> usize {
        match self {
            Role::Leader => 89,
            Role::Follower => 90,
        }
    }
}

/// Somewhere the peer might be reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// One of the peer's direct hints.
    Direct(SocketAddr),
    /// A transit relay.
    Relay {
        /// Its endpoints, tried in order.
        endpoints: Vec<Endpoint>,
        /// Ours, from the settings, rather than the peer's.
        ours: bool,
    },
}

/// The guards of one transfer, as the hints the library is given.
pub(crate) struct Plan {
    /// For `transit::init`: the library's own "relay hints".
    pub ours: Vec<RelayHint>,
    /// For `TransitConnector::connect`: the "peer's".
    pub theirs: Hints,
    /// Cancels the guards still waiting for the library.
    unused: CancellationToken,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    _stop: DropGuard,
}

impl Plan {
    /// The library has its connection: guards it has not come to are no
    /// longer needed.
    pub(crate) fn connected(&self) {
        self.unused.cancel();
    }

    /// Ends a transfer that went well: the library has let go of its
    /// connection, so the guard carrying it forwards what was last written
    /// (the transit ack) and stops. Waits at most `bound`; dropping the
    /// plan then stops whatever is left.
    pub(crate) async fn finish(mut self, bound: Duration) {
        self.unused.cancel();
        let tasks = std::mem::take(&mut self.tasks);
        let _ = tokio::time::timeout(bound, async {
            for t in tasks {
                let _ = t.await;
            }
        })
        .await;
    }
}

/// Where to try, best first: our relay, the peer's relay if it named a
/// different one, then its direct hints, private addresses first. At most
/// [`MAX_TARGETS`].
pub(crate) fn targets(
    our_relay: &Endpoint,
    their: &TheirTransit,
    reach: ReachPolicy,
) -> Vec<Target> {
    let mut out = vec![Target::Relay {
        endpoints: vec![our_relay.clone()],
        ours: true,
    }];
    if their.relay
        && let Some(relay) = their
            .relays
            .iter()
            .find(|endpoints| !endpoints.contains(our_relay))
    {
        out.push(Target::Relay {
            endpoints: relay.clone(),
            ours: false,
        });
    }
    if their.direct {
        let mut direct: Vec<SocketAddr> = their
            .direct_hints
            .iter()
            .filter(|(ip, _)| peer_may_name(*ip, reach))
            .map(|(ip, port)| SocketAddr::new(*ip, *port))
            .collect();
        direct.sort_by_key(|a| !is_local_net(a.ip()));
        out.extend(direct.into_iter().map(Target::Direct));
    }
    out.truncate(MAX_TARGETS);
    out
}

/// Whether the peer may make us connect to `ip`: nothing unspecified,
/// multicast or broadcast; loopback only when the reach policy allows it
/// (tests). The phone's own LAN address is not told apart, so a peer can
/// point one of its [`MAX_PEER_CONNECTS`] attempts at a service of ours
/// that listens on every interface; it only ever gets the fixed relay or
/// transit handshake bytes.
fn peer_may_name(ip: IpAddr, reach: ReachPolicy) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 @ IpAddr::V4(_) => v4,
    };
    if ip.is_loopback() {
        return reach.allow_loopback;
    }
    let special = match ip {
        IpAddr::V4(v4) => v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast(),
        IpAddr::V6(v6) => v6.is_unspecified() || v6.is_multicast(),
    };
    !special
}

/// Private, link-local or ULA: likely the same network, so tried first.
fn is_local_net(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

/// Starts one guard per target and returns the hints that point the
/// library at them. The guards stop when the plan is dropped or `parent`
/// is cancelled.
///
/// # Errors
///
/// No loopback port.
pub(crate) async fn plan(
    targets: Vec<Target>,
    relay_token: &str,
    role: Role,
    tuning: &Tuning,
    reach: ReachPolicy,
    parent: &CancellationToken,
) -> Result<Plan, ErrorInfo> {
    let stop = parent.child_token();
    let unused = stop.child_token();
    let peer_connects = Arc::new(AtomicUsize::new(0));
    let mut hints = Vec::with_capacity(targets.len());
    let mut tasks = Vec::with_capacity(targets.len());
    for target in targets.into_iter().take(MAX_TARGETS) {
        let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .map_err(|_| no_port())?;
        let port = listener.local_addr().map_err(|_| no_port())?.port();
        let guard = TransitGuard {
            target,
            prefix: format!("please relay {relay_token} for side "),
            role,
            tuning: *tuning,
            reach,
            peer_connects: peer_connects.clone(),
        };
        let running = stop.clone();
        let waiting = unused.clone();
        tasks.push(tokio::spawn(async move {
            tokio::select! {
                () = running.cancelled() => {}
                r = guard.run(listener, waiting) => {
                    if let Err(reason) = r {
                        tracing::debug!(reason, "transit guard closed");
                    }
                }
            }
        }));
        hints.push(RelayHint::new(
            None,
            [DirectHint::new(Ipv4Addr::LOCALHOST.to_string(), port)],
            [],
        ));
    }
    let theirs = hints.split_off(hints.len().min(2));
    Ok(Plan {
        ours: hints,
        theirs: Hints::new([], theirs),
        unused,
        tasks,
        _stop: stop.drop_guard(),
    })
}

fn no_port() -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Network, "no loopback port for transit")
}

struct TransitGuard {
    target: Target,
    /// The relay line up to the side, which is the library's own random.
    prefix: String,
    role: Role,
    tuning: Tuning,
    reach: ReachPolicy,
    /// Attempts made so far, by every guard of the transfer, to addresses
    /// the peer chose. Never given back.
    peer_connects: Arc<AtomicUsize>,
}

impl TransitGuard {
    async fn run(
        self,
        listener: TcpListener,
        unused: CancellationToken,
    ) -> Result<(), &'static str> {
        // The library starts every attempt at once, and gives up after its
        // own timeout; a guard that has not got as far as carrying a
        // handshake by then, or by the time the library has its connection
        // elsewhere, is not needed.
        let (local, remote) = tokio::select! {
            () = unused.cancelled() => return Err("unused"),
            r = self.open(listener) => r?,
        };
        splice(local, remote, self.role.peer_handshake_bytes()).await
    }

    /// Everything up to the splice: the library's connection, its relay
    /// line, the real connection, and the relay handshake.
    async fn open(&self, listener: TcpListener) -> Result<(TcpStream, TcpStream), &'static str> {
        let window = self.tuning.handshake;
        let (mut local, peer) = tokio::time::timeout(window, listener.accept())
            .await
            .map_err(|_| "unused")?
            .map_err(|_| "accept failed")?;
        drop(listener);
        if !peer.ip().is_loopback() {
            return Err("not loopback");
        }
        let line = tokio::time::timeout(window, read_line(&mut local))
            .await
            .map_err(|_| "no relay line")??;
        if !self.is_our_line(&line) {
            return Err("not the library");
        }
        let mut remote = tokio::time::timeout(window, self.connect())
            .await
            .map_err(|_| "connect timed out")??;
        let _ = remote.set_nodelay(true);
        let _ = local.set_nodelay(true);
        match &self.target {
            Target::Direct(_) => {
                // We are the relay the library thinks it reached.
                local.write_all(b"ok\n").await.map_err(|_| "local write")?;
            }
            Target::Relay { .. } => {
                remote.write_all(&line).await.map_err(|_| "relay write")?;
                let mut ok = [0u8; 3];
                tokio::time::timeout(window, remote.read_exact(&mut ok))
                    .await
                    .map_err(|_| "relay silent")?
                    .map_err(|_| "relay closed")?;
                if &ok != b"ok\n" {
                    return Err("relay refused");
                }
                local.write_all(&ok).await.map_err(|_| "local write")?;
            }
        }
        Ok((local, remote))
    }

    /// Whether `line` is "please relay <our token> for side <16 hex>\n".
    fn is_our_line(&self, line: &[u8]) -> bool {
        let Some(rest) = line.strip_prefix(self.prefix.as_bytes()) else {
            return false;
        };
        let Some(side) = rest.strip_suffix(b"\n") else {
            return false;
        };
        side.len() == 16 && side.iter().all(u8::is_ascii_hexdigit)
    }

    async fn connect(&self) -> Result<TcpStream, &'static str> {
        match &self.target {
            Target::Direct(addr) => self.peer_connect(*addr).await,
            // Our own relay is the user's choice: every endpoint and
            // address is tried until one answers.
            Target::Relay {
                endpoints,
                ours: true,
            } => {
                for ep in endpoints {
                    let Ok(addrs) = tokio::net::lookup_host((ep.host.as_str(), ep.port)).await
                    else {
                        continue;
                    };
                    for addr in addrs.take(MAX_ADDRS_PER_HOST) {
                        if let Ok(s) = TcpStream::connect(addr).await {
                            return Ok(s);
                        }
                    }
                }
                Err("relay unreachable")
            }
            // The peer's relay is held to the rule for its direct hints,
            // and gets one attempt: the first address it may name. Falling
            // back through its endpoints and their addresses would let one
            // relay hint probe a dozen addresses, one refusal at a time.
            Target::Relay {
                endpoints,
                ours: false,
            } => {
                for ep in endpoints {
                    let Ok(addrs) = tokio::net::lookup_host((ep.host.as_str(), ep.port)).await
                    else {
                        continue;
                    };
                    let first = addrs
                        .take(MAX_ADDRS_PER_HOST)
                        .find(|a| peer_may_name(a.ip(), self.reach));
                    if let Some(addr) = first {
                        return self.peer_connect(addr).await;
                    }
                }
                Err("relay unreachable")
            }
        }
    }

    /// One connection attempt to an address the peer chose, if the
    /// transfer has any left.
    async fn peer_connect(&self, addr: SocketAddr) -> Result<TcpStream, &'static str> {
        if !crate::slots::take(&self.peer_connects, MAX_PEER_CONNECTS) {
            return Err("no connection attempts left for the peer's addresses");
        }
        TcpStream::connect(addr)
            .await
            .map_err(|_| "peer unreachable")
    }
}

/// Reads one line of at most [`RELAY_LINE_BYTES`], byte by byte so nothing
/// after it is consumed.
async fn read_line(s: &mut TcpStream) -> Result<Vec<u8>, &'static str> {
    let mut line = Vec::with_capacity(RELAY_LINE_BYTES);
    loop {
        let b = s.read_u8().await.map_err(|_| "no relay line")?;
        line.push(b);
        if b == b'\n' {
            return Ok(line);
        }
        if line.len() >= RELAY_LINE_BYTES {
            return Err("relay line too long");
        }
    }
}

/// Joins the library to the peer. The library's bytes go out untouched;
/// the peer's pass once the handshake is through only as records of at most
/// [`MAX_RECORD_WIRE_BYTES`].
async fn splice(local: TcpStream, remote: TcpStream, handshake: usize) -> Result<(), &'static str> {
    let (mut local_r, local_w) = local.into_split();
    let (remote_r, mut remote_w) = remote.into_split();
    let outward = async {
        let _ = tokio::io::copy(&mut local_r, &mut remote_w).await;
        let _ = remote_w.shutdown().await;
    };
    let inward = forward_records(remote_r, local_w, handshake);
    tokio::select! {
        // The library closed its end: this connection lost, or the
        // transfer is over. Nothing more to carry either way.
        () = outward => Ok(()),
        r = inward => r,
    }
}

async fn forward_records(
    mut from: OwnedReadHalf,
    mut to: OwnedWriteHalf,
    handshake: usize,
) -> Result<(), &'static str> {
    let mut buf = vec![0u8; COPY_BYTES];
    copy_exact(&mut from, &mut to, &mut buf, handshake).await?;
    loop {
        let mut header = [0u8; 4];
        match from.read_exact(&mut header).await {
            Ok(_) => {}
            // The peer is done (or cut off mid-header, which the library
            // will see as a short stream).
            Err(_) => {
                let _ = to.shutdown().await;
                return Ok(());
            }
        }
        let len = u32::from_be_bytes(header);
        if len > MAX_RECORD_WIRE_BYTES {
            return Err("oversized transit record");
        }
        to.write_all(&header).await.map_err(|_| "local write")?;
        copy_exact(
            &mut from,
            &mut to,
            &mut buf,
            usize::try_from(len).unwrap_or(usize::MAX),
        )
        .await?;
    }
}

/// Copies exactly `n` bytes, or fewer if the peer stops, which ends the
/// splice.
async fn copy_exact(
    from: &mut OwnedReadHalf,
    to: &mut OwnedWriteHalf,
    buf: &mut [u8],
    mut n: usize,
) -> Result<(), &'static str> {
    while n > 0 {
        let want = n.min(buf.len());
        let chunk = buf.get_mut(..want).ok_or("buffer")?;
        let got = from.read(chunk).await.map_err(|_| "peer read")?;
        if got == 0 {
            let _ = to.shutdown().await;
            return Err("peer closed mid-record");
        }
        let data = chunk.get(..got).ok_or("buffer")?;
        to.write_all(data).await.map_err(|_| "local write")?;
        n = n.saturating_sub(got);
    }
    Ok(())
}

/// Tests of the target choice only; the guards are exercised end to end in
/// `tests/wormhole*.rs`.
#[cfg(test)]
mod tests {
    use super::*;

    fn ep(host: &str, port: u16) -> Endpoint {
        Endpoint {
            host: host.into(),
            port,
        }
    }

    #[test]
    fn targets_prefer_our_relay_then_theirs_then_private_direct() {
        let ours = ep("transit.magic-wormhole.io", 4001);
        let their = TheirTransit {
            direct: true,
            relay: true,
            direct_hints: vec![
                ("203.0.113.9".parse().unwrap(), 1),
                ("192.168.1.5".parse().unwrap(), 2),
                ("127.0.0.1".parse().unwrap(), 3),
                ("0.0.0.0".parse().unwrap(), 4),
                ("224.0.0.1".parse().unwrap(), 5),
                ("::ffff:127.0.0.1".parse().unwrap(), 6),
            ],
            relays: vec![vec![ours.clone()], vec![ep("other.example", 9)]],
        };
        let t = targets(&ours, &their, ReachPolicy::default());
        assert_eq!(
            t,
            vec![
                Target::Relay {
                    endpoints: vec![ours.clone()],
                    ours: true
                },
                Target::Relay {
                    endpoints: vec![ep("other.example", 9)],
                    ours: false
                },
                Target::Direct("192.168.1.5:2".parse().unwrap()),
                Target::Direct("203.0.113.9:1".parse().unwrap()),
            ]
        );
        let t = targets(
            &ours,
            &TheirTransit {
                direct: true,
                direct_hints: vec![("127.0.0.1".parse().unwrap(), 3)],
                ..TheirTransit::default()
            },
            ReachPolicy {
                allow_loopback: true,
            },
        );
        assert_eq!(t.len(), 2, "loopback only when the policy allows it");
        let t = targets(
            &ours,
            &TheirTransit {
                direct: false,
                direct_hints: vec![("192.168.1.5".parse().unwrap(), 3)],
                ..TheirTransit::default()
            },
            ReachPolicy::default(),
        );
        assert_eq!(t.len(), 1, "hints without the ability are not used");
    }

    /// A loopback listener that counts the connections it accepts.
    async fn counting() -> (u16, Arc<AtomicUsize>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                drop(s);
            }
        });
        (port, hits)
    }

    /// A loopback port nothing listens on: connecting is refused.
    async fn refusing() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    }

    fn guard(target: Target, peer_connects: &Arc<AtomicUsize>) -> TransitGuard {
        TransitGuard {
            target,
            prefix: String::new(),
            role: Role::Follower,
            tuning: Tuning::default(),
            reach: ReachPolicy {
                allow_loopback: true,
            },
            peer_connects: peer_connects.clone(),
        }
    }

    fn hits(h: &Arc<AtomicUsize>) -> usize {
        h.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[tokio::test]
    async fn the_peer_gets_three_connection_attempts_in_all() {
        // The worst the peer can name: a relay with three endpoints whose
        // first refuses, and more direct hints than fit.
        let (a, a_hits) = counting().await;
        let (b, b_hits) = counting().await;
        let mut direct = Vec::new();
        for _ in 0..3 {
            direct.push(counting().await);
        }
        let their = TheirTransit {
            direct: true,
            relay: true,
            direct_hints: direct
                .iter()
                .map(|(p, _)| ("127.0.0.1".parse().unwrap(), *p))
                .collect(),
            relays: vec![vec![
                ep("127.0.0.1", refusing().await),
                ep("127.0.0.1", a),
                ep("127.0.0.1", b),
            ]],
        };
        let ours = ep("127.0.0.1", refusing().await);
        let tried = Arc::new(AtomicUsize::new(0));
        let reach = ReachPolicy {
            allow_loopback: true,
        };
        for target in targets(&ours, &their, reach) {
            let _ = guard(target, &tried).connect().await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        // The relay had its one attempt, refused; it fell back to nothing.
        assert_eq!(hits(&a_hits) + hits(&b_hits), 0);
        // Two direct hints fit beside it, one attempt each: three in all.
        let direct_hits: usize = direct.iter().map(|(_, h)| hits(h)).sum();
        assert_eq!(direct_hits, 2);
        assert_eq!(hits(&tried), MAX_PEER_CONNECTS);
        // However the targets were chosen, a fourth is not made.
        let (d, d_hits) = counting().await;
        let fourth = guard(Target::Direct(([127, 0, 0, 1], d).into()), &tried);
        assert!(fourth.connect().await.is_err());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(hits(&d_hits), 0);
        // Our own relay is not the peer's choice, and is not counted.
        let (o, o_hits) = counting().await;
        let own = Target::Relay {
            endpoints: vec![ep("127.0.0.1", refusing().await), ep("127.0.0.1", o)],
            ours: true,
        };
        assert!(guard(own, &tried).connect().await.is_ok());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(hits(&o_hits), 1);
    }

    #[test]
    fn only_the_librarys_relay_line_is_accepted() {
        let g = TransitGuard {
            target: Target::Direct("192.0.2.1:1".parse().unwrap()),
            prefix: format!("please relay {} for side ", "ab".repeat(32)),
            role: Role::Follower,
            tuning: Tuning::default(),
            reach: ReachPolicy::default(),
            peer_connects: Arc::default(),
        };
        let good = format!(
            "please relay {} for side 0123456789abcdef\n",
            "ab".repeat(32)
        );
        assert_eq!(good.len(), RELAY_LINE_BYTES);
        assert!(g.is_our_line(good.as_bytes()));
        let wrong_token = format!(
            "please relay {} for side 0123456789abcdef\n",
            "cd".repeat(32)
        );
        assert!(!g.is_our_line(wrong_token.as_bytes()));
        let short_side = format!("please relay {} for side 0123\n", "ab".repeat(32));
        assert!(!g.is_our_line(short_side.as_bytes()));
        assert!(!g.is_our_line(b"GET / HTTP/1.1\r\n"));
    }

    #[test]
    fn the_handshake_lengths_match_the_protocol() {
        let hex = "ab".repeat(32);
        assert_eq!(
            format!("transit receiver {hex} ready\n\n").len(),
            Role::Leader.peer_handshake_bytes()
        );
        assert_eq!(
            format!("transit sender {hex} ready\n\ngo\n").len(),
            Role::Follower.peer_handshake_bytes()
        );
    }
}
