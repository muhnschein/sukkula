//! mDNS for real: the daemons on the loopback interface and a UDP port of
//! the test's own instead of 5353, so nothing is announced on, or read
//! from, the network the tests run on.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mdns_sd::{IfKind, Receiver, ResolvedService, ServiceDaemon, ServiceEvent, SourcePredicate};
use rqs_lib::hdl::{AddrFilter, MAX_ENDPOINTS_PER_SOURCE, SERVICE_TYPE};
use rqs_lib::utils::{gen_mdns_endpoint_info, gen_mdns_name, parse_mdns_endpoint_info, parse_mdns_name};
use rqs_lib::{DeviceType, EndpointInfo, MDnsDiscovery, MDnsServer};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// How long anything here may take: registration probes for 750 ms first.
const WAIT: Duration = Duration::from_secs(10);

/// A port no other test uses at the same time, most likely.
fn test_port() -> u16 {
    rand::random_range(20_000..60_000)
}

/// The reach policy, as the tests' embedding application gives it: loopback.
fn loopback() -> AddrFilter {
    Arc::new(|ip: IpAddr| ip.is_loopback())
}

/// Another mDNS daemon, on loopback and `port`: the peer that looks for us.
fn browser(port: u16) -> ServiceDaemon {
    let daemon = ServiceDaemon::new_with_port(port).unwrap();
    daemon.disable_interface(IfKind::All).unwrap();
    daemon.enable_interface(IfKind::LoopbackV4).unwrap();
    daemon
}

/// Waits for `fullname` to be resolved on `events`.
async fn resolved(events: &Receiver<ServiceEvent>, fullname: &str) -> ResolvedService {
    let found = async {
        loop {
            if let ServiceEvent::ServiceResolved(info) = events.recv_async().await.unwrap() {
                if info.get_fullname() == fullname {
                    return *info;
                }
            }
        }
    };
    tokio::time::timeout(WAIT, found)
        .await
        .unwrap_or_else(|_| panic!("{fullname} was not resolved within {WAIT:?}"))
}

/// Waits for `fullname` to be reported gone on `events`.
async fn removed(events: &Receiver<ServiceEvent>, fullname: &str) {
    let gone = async {
        loop {
            if let ServiceEvent::ServiceRemoved(_, name) = events.recv_async().await.unwrap() {
                if name == fullname {
                    return;
                }
            }
        }
    };
    tokio::time::timeout(WAIT, gone)
        .await
        .unwrap_or_else(|_| panic!("{fullname} was not removed within {WAIT:?}"));
}

// F-QS1, F-QS4: the announcement that makes the device visible as a
// receiver. It used to be refused by mdns-sd (a host name not ending in
// ".local."), silently, so no peer ever saw it.
#[tokio::test]
async fn the_announcement_registers_and_another_daemon_resolves_it() {
    let port = test_port();
    let peer = browser(port);
    let events = peer.browse(SERVICE_TYPE).unwrap();

    let mut server = MDnsServer::with_port(
        port,
        *b"Ab3z",
        4242,
        "Test laptop",
        DeviceType::Laptop,
        loopback(),
    )
    .expect("the service is registered");
    let fullname = server.fullname().to_string();
    assert_eq!(parse_mdns_name(&fullname), Some(*b"Ab3z"));
    let ctk = CancellationToken::new();
    let task = tokio::spawn({
        let ctk = ctk.clone();
        async move { server.run(ctk).await }
    });

    let info = resolved(&events, &fullname).await;
    assert_eq!(info.get_port(), 4242);
    assert!(info.get_hostname().ends_with(".local."), "{}", info.get_hostname());
    assert!(info.get_addresses_v4().contains(&Ipv4Addr::LOCALHOST));
    let n = info.get_property_val_str("n").expect("the endpoint info");
    let (device_type, name) = parse_mdns_endpoint_info(n).unwrap();
    assert_eq!(device_type, DeviceType::Laptop);
    assert_eq!(name, "Test laptop");

    // Stopping says goodbye: the peer forgets us at once, not at expiry.
    ctk.cancel();
    tokio::time::timeout(WAIT, task).await.unwrap().unwrap().unwrap();
    removed(&events, &fullname).await;
    peer.shutdown().unwrap();
}

/// DNS messages as a peer on the link would send them, built by hand: the
/// tests choose every byte, which is the point.
mod wire {
    pub const PTR: u16 = 12;
    pub const TXT: u16 = 16;
    pub const SRV: u16 = 33;
    pub const A: u16 = 1;
    const IN: u16 = 1;
    const FLUSH: u16 = 0x8000;

    pub fn name(out: &mut Vec<u8>, name: &str) {
        for label in name.trim_end_matches('.').split('.') {
            out.push(label.len() as u8);
            out.extend_from_slice(label.as_bytes());
        }
        out.push(0);
    }

    /// One resource record.
    pub struct Record {
        pub name: String,
        pub ty: u16,
        pub ttl: u32,
        pub rdata: Vec<u8>,
    }

    pub fn ptr(ty: &str, instance: &str, ttl: u32) -> Record {
        let mut rdata = Vec::new();
        name(&mut rdata, instance);
        Record { name: ty.to_string(), ty: PTR, ttl, rdata }
    }

    pub fn srv(instance: &str, host: &str, port: u16, ttl: u32) -> Record {
        let mut rdata = vec![0, 0, 0, 0];
        rdata.extend_from_slice(&port.to_be_bytes());
        name(&mut rdata, host);
        Record { name: instance.to_string(), ty: SRV, ttl, rdata }
    }

    pub fn txt(instance: &str, entry: &str, ttl: u32) -> Record {
        let mut rdata = vec![entry.len() as u8];
        rdata.extend_from_slice(entry.as_bytes());
        Record { name: instance.to_string(), ty: TXT, ttl, rdata }
    }

    pub fn a(host: &str, ip: std::net::Ipv4Addr, ttl: u32) -> Record {
        Record { name: host.to_string(), ty: A, ttl, rdata: ip.octets().to_vec() }
    }

    /// A response carrying `records` as answers.
    pub fn response(records: &[Record]) -> Vec<u8> {
        let mut out = vec![0, 0, 0x84, 0];
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&(records.len() as u16).to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        for r in records {
            name(&mut out, &r.name);
            out.extend_from_slice(&r.ty.to_be_bytes());
            let class = if r.ty == PTR { IN } else { IN | FLUSH };
            out.extend_from_slice(&class.to_be_bytes());
            out.extend_from_slice(&r.ttl.to_be_bytes());
            out.extend_from_slice(&(r.rdata.len() as u16).to_be_bytes());
            out.extend_from_slice(&r.rdata);
        }
        out
    }

    /// A one-question PTR query with message id `id`.
    pub fn ptr_query(ty: &str, id: u16) -> Vec<u8> {
        let mut out = id.to_be_bytes().to_vec();
        out.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        name(&mut out, ty);
        out.extend_from_slice(&PTR.to_be_bytes());
        out.extend_from_slice(&IN.to_be_bytes());
        out
    }
}

/// A host on the link at `ip`, sending from the mDNS port `port` as a
/// responder does (a response from any other port is ignored), to the
/// group on the loopback interface, where the daemons under test listen.
struct Host {
    socket: Socket,
    group: SocketAddr,
}

impl Host {
    fn new(ip: Ipv4Addr, port: u16) -> Host {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        socket.set_reuse_address(true).unwrap();
        socket.set_reuse_port(true).unwrap();
        socket.bind(&SocketAddr::from(SocketAddrV4::new(ip, port)).into()).unwrap();
        socket.set_multicast_if_v4(&Ipv4Addr::LOCALHOST).unwrap();
        socket.set_multicast_loop_v4(true).unwrap();
        let group = SocketAddr::from((Ipv4Addr::new(224, 0, 0, 251), port));
        Host { socket, group }
    }

    fn send(&self, packet: &[u8]) {
        self.socket.send_to(packet, &self.group.into()).unwrap();
    }

    /// Announces one Quick Share device: endpoint id `id`, at this host's
    /// address, with every record's TTL `ttl`.
    fn announce(&self, id: [u8; 4], at: Ipv4Addr, ttl: u32) {
        let instance = format!("{}.{SERVICE_TYPE}", gen_mdns_name(id));
        let host = format!("{}.local.", gen_mdns_name(id));
        let n = format!("n={}", gen_mdns_endpoint_info(DeviceType::Phone as u8, "Phone"));
        self.send(&wire::response(&[
            wire::ptr(SERVICE_TYPE, &instance, ttl),
            wire::srv(&instance, &host, 4000, ttl),
            wire::txt(&instance, &n, ttl),
            wire::a(&host, at, ttl),
        ]));
    }
}

/// `n` distinct endpoint ids, letters only.
fn ids(n: usize) -> Vec<[u8; 4]> {
    (0..n)
        .map(|i| {
            let d = |k: usize| b'a' + ((i / 26usize.pow(k as u32)) % 26) as u8;
            [b'Z', d(2), d(1), d(0)]
        })
        .collect()
}

/// What `rx` reports until `until` holds of it or `WAIT` is over: the
/// services present, by full name, with their sources.
async fn watch(
    rx: &mut broadcast::Receiver<EndpointInfo>,
    present: &mut HashMap<String, Option<IpAddr>>,
    mut until: impl FnMut(&HashMap<String, Option<IpAddr>>) -> bool,
) -> bool {
    let deadline = Instant::now() + WAIT;
    while !until(present) {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Ok(ei)) => {
                if ei.present == Some(true) {
                    present.insert(ei.fullname, ei.source);
                } else {
                    present.remove(&ei.fullname);
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(e)) => panic!("{e}"),
            Err(_) => return false,
        }
    }
    true
}

// S7 (kept[15]): the responder answers only the reach policy's sources, and
// its unicast replies are limited per address. Legacy-unicast replies go to
// whatever address and port a query came from, so without this any host
// could make it send its name and port anywhere, as fast as it liked.
#[tokio::test]
async fn the_responder_answers_only_the_policy_and_not_without_limit() {
    let port = test_port();
    let only_one = Ipv4Addr::LOCALHOST;
    let filter: AddrFilter = Arc::new(move |ip: IpAddr| ip == IpAddr::V4(only_one));
    let mut server =
        MDnsServer::with_port(port, *b"Rspd", 4243, "Responder", DeviceType::Phone, filter)
            .unwrap();
    let fullname = server.fullname().to_string();
    let ctk = CancellationToken::new();
    let task = tokio::spawn({
        let ctk = ctk.clone();
        async move { server.run(ctk).await }
    });
    // Once another daemon has resolved it, it is announced and answers.
    let peer = browser(port);
    resolved(&peer.browse(SERVICE_TYPE).unwrap(), &fullname).await;
    // Alone on the port from here on, so a unicast query reaches it.
    peer.shutdown().unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let to = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let answers = |from: Ipv4Addr, queries: u16| {
        let s = UdpSocket::bind((from, 0)).unwrap();
        s.set_read_timeout(Some(Duration::from_millis(1500))).unwrap();
        for id in 0..queries {
            s.send_to(&wire::ptr_query(SERVICE_TYPE, 0x5000 + id), to).unwrap();
        }
        let mut got = 0;
        let mut buf = [0u8; 9000];
        while let Ok((len, _)) = s.recv_from(&mut buf) {
            // A reply: our id, and the answer names the service.
            assert!(len > 12 && buf[0] == 0x50);
            got += 1;
        }
        got
    };
    // From outside the policy: nothing, not even to a reachable address.
    assert_eq!(answers(Ipv4Addr::new(127, 0, 0, 2), 3), 0);
    // From inside: answered -- but a burst gets no more than four a second.
    assert_eq!(answers(only_one, 1), 1);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let burst = answers(only_one, 12);
    assert!((1..=4).contains(&burst), "{burst} replies to a burst of 12");

    ctk.cancel();
    tokio::time::timeout(WAIT, task).await.unwrap().unwrap().unwrap();
}

// kept[1], kept[6]: one host on the link announcing without end -- 1500
// devices at the longest TTL there is, and records for names nobody asked
// about -- neither grows what the daemon keeps nor keeps a device it did
// not announce off the list. The list used to be first come, first
// served, for as long as the first comer kept announcing, and the cache
// under it took everything.
#[tokio::test]
async fn a_flood_from_one_host_is_bounded_and_keeps_nobody_else_out() {
    let port = test_port();
    let (tx, mut rx) = broadcast::channel(1024);
    let discovery = MDnsDiscovery::with_port(port, tx, loopback()).unwrap();
    let ctk = CancellationToken::new();
    let task = tokio::spawn(discovery.run(ctk.clone()));
    // A second daemon browsing as MDnsDiscovery does, for its counters.
    let counted = ServiceDaemon::new_with_source_filter(
        port,
        SourcePredicate::new(|ip: &IpAddr| ip.is_loopback()),
    )
    .unwrap();
    counted.disable_interface(IfKind::All).unwrap();
    counted.enable_interface(IfKind::LoopbackV4).unwrap();
    // Its events are read, or it would stop at the tenth, waiting.
    let counted_events = counted.browse(SERVICE_TYPE).unwrap();
    let drain = tokio::spawn(async move { while counted_events.recv_async().await.is_ok() {} });
    // Both are browsing once their first query has gone out.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let attacker_ip = Ipv4Addr::new(127, 0, 0, 2);
    let attacker = Host::new(attacker_ip, port);
    for (i, id) in ids(1500).into_iter().enumerate() {
        attacker.announce(id, attacker_ip, u32::MAX);
        // Records for names no browsed instance leads to.
        let junk = format!("junk{i}.local.");
        attacker.send(&wire::response(&[
            wire::srv(&format!("x{i}._other._tcp.local."), &junk, 1, u32::MAX),
            wire::txt(&format!("x{i}._other._tcp.local."), "a=b", u32::MAX),
            wire::a(&junk, attacker_ip, u32::MAX),
        ]));
        if i % 100 == 99 {
            // At the pace a daemon keeps up with; drops by the kernel prove
            // nothing either way.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    // An honest phone on the same link announces itself, the ordinary way.
    let phone_ip = Ipv4Addr::new(127, 0, 0, 3);
    let phone = Host::new(phone_ip, port);
    let phone_id = *b"Hnst";
    let phone_name = format!("{}.{SERVICE_TYPE}", gen_mdns_name(phone_id));
    let mut present = HashMap::new();
    let mut listed = false;
    for _ in 0..3 {
        phone.announce(phone_id, phone_ip, 120);
        if watch(&mut rx, &mut present, |p| p.contains_key(&phone_name)).await {
            listed = true;
            break;
        }
    }
    assert!(listed, "the phone was never listed; present: {present:?}");
    assert_eq!(present[&phone_name], Some(IpAddr::V4(phone_ip)));
    let from_attacker = present
        .values()
        .filter(|s| **s == Some(IpAddr::V4(attacker_ip)))
        .count();
    assert!(from_attacker <= MAX_ENDPOINTS_PER_SOURCE, "{from_attacker} listed from one host");

    // What the daemon under it keeps is bounded too: a few instances of the
    // attacker's and the phone's, their records, and hardly any timers --
    // not 1500 of each, and not a timer per record per packet.
    let metrics = tokio::time::timeout(WAIT, counted.get_metrics().unwrap().recv_async())
        .await
        .expect("the daemon answers")
        .unwrap();
    let get = |k: &str| metrics.get(k).copied().unwrap_or(0);
    assert!(get("cached-ptr") <= 5, "{metrics:?}");
    assert!(get("cached-srv") <= 10, "{metrics:?}");
    assert!(get("cached-txt") <= 10, "{metrics:?}");
    assert!(get("cached-addr") <= 10, "{metrics:?}");
    assert!(get("timer") < 64, "{metrics:?}");
    counted.shutdown().unwrap();
    let _ = tokio::time::timeout(WAIT, drain).await;

    ctk.cancel();
    tokio::time::timeout(WAIT, task).await.unwrap().unwrap().unwrap();
}
