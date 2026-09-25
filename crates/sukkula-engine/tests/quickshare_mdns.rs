//! Quick Share over mDNS for real (F-QS1, F-QS4, S7): the daemons on the
//! loopback interface only (the test switch, `StartConfig.allow_loopback`)
//! and on a UDP port of each test's own instead of 5353, so nothing is
//! announced on, or read from, the network the tests run on.
//!
//! - Receiving registers the announcement, and another mDNS daemon on the
//!   link resolves it to the listener. Every other Quick Share test turns
//!   mDNS off, and the announcement mdns-sd refused (a host name without
//!   ".local.") went unnoticed for that reason (review kept[0]).
//! - Discovery survives a host that announces without end: it lists a few
//!   of that host's services, never grows past its bounds, and a phone
//!   that announces itself afterwards is listed (kept[1], kept[6]).

#![cfg(feature = "quickshare")]
// Fixture code outside `#[test]` functions, where clippy's test allowances
// do not reach. A fixture that cannot be built is a broken test.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_possible_truncation
)]

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent};
use rqs_lib::hdl::SERVICE_TYPE;
use rqs_lib::utils::{gen_mdns_endpoint_info, gen_mdns_name, parse_mdns_endpoint_info};
use socket2::{Domain, Protocol, Socket, Type};
use sukkula_core::config::Settings;
use sukkula_core::consent::ConsentBroker;
use sukkula_core::inbox::Inbox;
use sukkula_core::reach::ReachPolicy;
use sukkula_core::store::Store;
use sukkula_engine::adapter::Adapter;
use sukkula_engine::api::Event;
use sukkula_engine::ctx::Ctx;
use sukkula_engine::quickshare::{Options, QuickShareAdapter, adapter_with_mdns_port};

/// How long anything here may take: registration probes for 750 ms first.
const WAIT: Duration = Duration::from_secs(10);

/// A port no other test uses at the same time, most likely.
fn test_port() -> u16 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    20_000 + ((nanos ^ std::process::id()) % 40_000) as u16
}

struct Rig {
    _dir: tempfile::TempDir,
    adapter: Arc<QuickShareAdapter>,
    events: Arc<Mutex<Vec<Event>>>,
}

impl Rig {
    fn new(device_name: &str, mdns_port: u16) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let consent = ConsentBroker::with_limits(Arc::new(|_| {}), Duration::from_secs(10), 2);
        let settings = Settings {
            device_name: device_name.to_owned(),
            ..Settings::default()
        };
        let e = events.clone();
        let ctx = Arc::new(Ctx::new(
            settings,
            "Test Phone".into(),
            Store::open(&dir.path().join("data")).unwrap(),
            Inbox::open(&dir.path().join("dl")).unwrap(),
            consent,
            ReachPolicy {
                allow_loopback: true,
            },
            Arc::new(move |ev| e.lock().unwrap().push(ev)),
        ));
        let adapter = adapter_with_mdns_port(
            ctx,
            Options {
                mdns: true,
                listen: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
                system_bus: None,
                ..Options::default()
            },
            mdns_port,
        );
        Rig {
            _dir: dir,
            adapter,
            events,
        }
    }

    /// The peers listed now, by id, from the events so far.
    fn listed(&self) -> HashSet<String> {
        let mut listed = HashSet::new();
        for e in self.events.lock().unwrap().iter() {
            match e {
                Event::PeerFound { peer } => {
                    listed.insert(peer.id.clone());
                }
                Event::PeerLost { peer } => {
                    listed.remove(peer);
                }
                _ => {}
            }
        }
        listed
    }
}

/// Another mDNS daemon on the link (loopback, `port`): the phone that looks
/// for us.
fn browser(port: u16) -> ServiceDaemon {
    let daemon = ServiceDaemon::new_with_port(port).unwrap();
    daemon.disable_interface(IfKind::All).unwrap();
    daemon.enable_interface(IfKind::LoopbackV4).unwrap();
    daemon
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiving_announces_the_phone_and_a_second_daemon_resolves_it() {
    let port = test_port();
    let peer = browser(port);
    let events = peer.browse(SERVICE_TYPE).unwrap();
    let rig = Rig::new("Announced Phone", port);

    // Registration is part of starting: had mdns-sd refused it, this would
    // be the error, and Quick Share would show as failed.
    rig.adapter.start_receiving().await.unwrap();
    let listening = rig.adapter.local_port().await.expect("listening");

    let found = tokio::time::timeout(WAIT, async {
        loop {
            if let ServiceEvent::ServiceResolved(info) = events.recv_async().await.unwrap() {
                return info;
            }
        }
    })
    .await
    .expect("the announcement was resolved");
    assert_eq!(found.get_port(), listening);
    assert!(found.get_hostname().ends_with(".local."));
    assert!(found.get_addresses_v4().contains(&Ipv4Addr::LOCALHOST));
    let (_, name) =
        parse_mdns_endpoint_info(found.get_property_val_str("n").expect("the endpoint info"))
            .unwrap();
    assert_eq!(name, "Announced Phone");

    // Stopping says goodbye.
    let fullname = found.get_fullname().to_string();
    rig.adapter.stop_receiving().await;
    tokio::time::timeout(WAIT, async {
        loop {
            if let ServiceEvent::ServiceRemoved(_, name) = events.recv_async().await.unwrap() {
                if name == fullname {
                    return;
                }
            }
        }
    })
    .await
    .expect("the goodbye arrived");
    peer.shutdown().unwrap();
}

/// A host on the link at `ip`, sending from the mDNS port as a responder
/// does, to the group on the loopback interface.
struct Host {
    socket: Socket,
    group: SocketAddr,
}

impl Host {
    fn new(ip: Ipv4Addr, port: u16) -> Host {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        socket.set_reuse_address(true).unwrap();
        socket.set_reuse_port(true).unwrap();
        socket
            .bind(&SocketAddr::from(SocketAddrV4::new(ip, port)).into())
            .unwrap();
        socket.set_multicast_if_v4(&Ipv4Addr::LOCALHOST).unwrap();
        Host {
            socket,
            group: SocketAddr::from((Ipv4Addr::new(224, 0, 0, 251), port)),
        }
    }

    /// Announces a Quick Share device with endpoint id `id` at `at`.
    fn announce(&self, id: [u8; 4], name: &str, at: Ipv4Addr, ttl: u32) {
        let instance = format!("{}.{SERVICE_TYPE}", gen_mdns_name(id));
        let host = format!("{}.local.", gen_mdns_name(id));
        let n = format!("n={}", gen_mdns_endpoint_info(2, name));
        let mut packet = vec![0, 0, 0x84, 0, 0, 0, 0, 4, 0, 0, 0, 0];
        let mut rdata = Vec::new();
        dns_name(&mut rdata, &instance);
        record(&mut packet, SERVICE_TYPE, 12, 1, ttl, &rdata);
        let mut rdata = vec![0, 0, 0, 0, 0x0f, 0xa0];
        dns_name(&mut rdata, &host);
        record(&mut packet, &instance, 33, 0x8001, ttl, &rdata);
        let mut rdata = vec![n.len() as u8];
        rdata.extend_from_slice(n.as_bytes());
        record(&mut packet, &instance, 16, 0x8001, ttl, &rdata);
        record(&mut packet, &host, 1, 0x8001, ttl, &at.octets());
        self.socket.send_to(&packet, &self.group.into()).unwrap();
    }
}

fn dns_name(out: &mut Vec<u8>, name: &str) {
    for label in name.trim_end_matches('.').split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
}

fn record(out: &mut Vec<u8>, name: &str, ty: u16, class: u16, ttl: u32, rdata: &[u8]) {
    dns_name(out, name);
    out.extend_from_slice(&ty.to_be_bytes());
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    out.extend_from_slice(rdata);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_announcing_without_end_keeps_no_phone_off_the_send_page() {
    let port = test_port();
    let rig = Rig::new("Looking", port);
    rig.adapter.start_discovery().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 1000 devices from one host; some of them name the phone's address,
    // to spend its budget as the rate limit used to count by that.
    let attacker_ip = Ipv4Addr::new(127, 0, 0, 2);
    let phone_ip = Ipv4Addr::new(127, 0, 0, 3);
    let attacker = Host::new(attacker_ip, port);
    for i in 0..1000usize {
        let id = [
            b'Z',
            b'a' + (i / 676 % 26) as u8,
            b'a' + (i / 26 % 26) as u8,
            b'a' + (i % 26) as u8,
        ];
        let at = if i % 2 == 0 { attacker_ip } else { phone_ip };
        attacker.announce(id, "Phone", at, u32::MAX);
        if i % 100 == 99 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let from_attacker = rig
        .listed()
        .iter()
        .filter(|id| id.starts_with("qs:Z"))
        .count();
    assert!(
        from_attacker <= 4,
        "{from_attacker} of one host's devices listed"
    );

    // The phone announces itself, from its own address, and is listed well
    // within the rate limit's window: its budget is its own to spend.
    let phone = Host::new(phone_ip, port);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !rig.listed().contains("qs:Hnst") {
        assert!(
            Instant::now() < deadline,
            "the phone was never listed: {:?}",
            rig.listed()
        );
        phone.announce(*b"Hnst", "Honest phone", phone_ip, 120);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    rig.adapter.stop_discovery().await;
}
