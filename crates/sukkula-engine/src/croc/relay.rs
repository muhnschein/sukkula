//! A croc relay, from the client's side (`src/tcp/tcp.go`).
//!
//! Every connection to a relay -- the control connection, and each data
//! connection after the key -- starts the same way: a PAKE on SIEC255 with
//! the public password `{1, 2, 3}`, a salt, and then, sealed under the key
//! that gives, the relay's password, the relay's banner (the ports of its
//! data connections), the room, and the relay's `ok`. The PAKE does not
//! authenticate the relay -- anyone knows the password -- it only keeps
//! the relay password and the room from passive eyes on the way.
//!
//! Two clients in one room are joined: the relay copies bytes between
//! them and reads nothing more. Until the second comes, it sends the
//! first a keepalive every second.
//!
//! What the relay says is not trusted: every frame of the handshake is
//! small and bounded, the banner must be a short list of ports, and the
//! answer to the room must be `ok` exactly.

use std::time::Duration;

use sukkula_core::config::{CrocSettings, relay_host_port};

use super::conn::{Conn, network};
use super::crypt::{self, Cipher};
use super::pake::{Curve, MAX_PAKE_BYTES, Pake};
use super::random;
use crate::api::{ErrorCode, ErrorInfo};

/// croc's public relay (`src/models/constants.go`).
pub(super) const DEFAULT_HOST: &str = "croc.schollz.com";

/// croc's relay port.
pub(super) const DEFAULT_PORT: u16 = 9009;

/// croc's default relay password.
pub(super) const DEFAULT_PASSWORD: &str = "pass123";

/// Most data ports taken from a banner. croc's relay has four.
pub(super) const MAX_BANNER_PORTS: usize = 16;

/// Longest handshake frame after the PAKE: a sealed banner and address.
const MAX_HANDSHAKE_FRAME: usize = 1024;

/// The relay PAKE's public password.
const RELAY_PAKE_PASSWORD: &[u8] = &[1, 2, 3];

/// Where the relay is, and its password.
#[derive(Clone)]
pub(super) struct Relay {
    /// Host name or address.
    pub(super) host: String,
    /// The control port.
    pub(super) port: u16,
    password: String,
}

impl std::fmt::Debug for Relay {
    /// S9: the password never shows.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relay")
            .field("host", &self.host)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl Relay {
    /// The relay the settings name, or croc's own.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::BadSettings`]: the relay does not parse (the settings
    /// were checked when saved; this checks again at every use).
    pub(super) fn from_settings(s: &CrocSettings) -> Result<Relay, ErrorInfo> {
        let (host, port) = match &s.relay {
            None => (DEFAULT_HOST.to_owned(), DEFAULT_PORT),
            Some(r) => relay_host_port(r)
                .ok_or_else(|| ErrorInfo::new(ErrorCode::BadSettings, "the croc relay"))?,
        };
        Ok(Relay {
            host,
            port,
            password: s
                .password
                .clone()
                .unwrap_or_else(|| DEFAULT_PASSWORD.to_owned()),
        })
    }

    /// A relay at `host:port` with croc's default password, for tests.
    #[cfg(test)]
    pub(super) fn at(host: &str, port: u16, password: &str) -> Relay {
        Relay {
            host: host.to_owned(),
            port,
            password: password.to_owned(),
        }
    }
}

/// A connection the relay has put in a room.
#[derive(Debug)]
pub(super) struct Joined {
    /// The connection, from here on the peer's.
    pub(super) conn: Conn,
    /// The data ports the relay offered.
    pub(super) ports: Vec<u16>,
}

/// Connects to `port` on the relay and joins `room`, all within `limit`.
/// Frames on the connection may be `max` bytes long.
///
/// # Errors
///
/// [`ErrorCode::Network`] for a relay that does not answer or does not
/// speak croc, [`ErrorCode::BadSettings`] for one that refuses the
/// password, [`ErrorCode::BadCode`] for a room that is taken.
pub(super) async fn join(
    relay: &Relay,
    port: u16,
    room: &str,
    max: usize,
    limit: Duration,
) -> Result<Joined, ErrorInfo> {
    let joining = async {
        let mut conn = Conn::open(&relay.host, port, max, limit).await?;
        let ports = handshake(&mut conn, &relay.password, room, limit).await?;
        Ok(Joined { conn, ports })
    };
    tokio::time::timeout(limit, joining)
        .await
        .map_err(|_| network("the croc relay does not answer"))?
}

/// F1 to F7 on a connection already open.
pub(super) async fn handshake(
    conn: &mut Conn,
    password: &str,
    room: &str,
    limit: Duration,
) -> Result<Vec<u16>, ErrorInfo> {
    let not_croc = || network("the croc relay does not speak croc");
    let mut pake = Pake::start(Curve::Siec, RELAY_PAKE_PASSWORD, random()?).ok_or_else(not_croc)?;
    conn.send(&pake.message(), limit).await?;
    let theirs = conn.recv(limit).await?;
    if theirs.len() > MAX_PAKE_BYTES {
        return Err(not_croc());
    }
    pake.finish(&theirs).map_err(|_| not_croc())?;
    let key = pake.key().ok_or_else(not_croc)?;
    let salt: [u8; crypt::SALT_BYTES] = random()?;
    let cipher = Cipher::new(&crypt::derive(&key, &salt));
    conn.send(&salt, limit).await?;
    conn.send(&seal(&cipher, password.as_bytes())?, limit)
        .await?;
    let banner = open(&cipher, &conn.recv(limit).await?).ok_or_else(not_croc)?;
    let ports = parse_banner(&banner).map_err(|e| match e {
        Banner::BadPassword => ErrorInfo::new(
            ErrorCode::BadSettings,
            "the croc relay refused its password",
        ),
        Banner::Malformed => not_croc(),
    })?;
    conn.send(&seal(&cipher, room.as_bytes())?, limit).await?;
    match open(&cipher, &conn.recv(limit).await?).as_deref() {
        Some(b"ok") => Ok(ports),
        Some(b"room full") => Err(ErrorInfo::new(
            ErrorCode::BadCode,
            "two others are using this code on the relay",
        )),
        _ => Err(not_croc()),
    }
}

fn seal(cipher: &Cipher, plaintext: &[u8]) -> Result<Vec<u8>, ErrorInfo> {
    cipher
        .seal(plaintext, random()?)
        .ok_or_else(|| network("sealing failed"))
}

fn open(cipher: &Cipher, frame: &[u8]) -> Option<Vec<u8>> {
    if frame.len() > MAX_HANDSHAKE_FRAME {
        return None;
    }
    cipher.open(frame)
}

/// Why a banner was not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Banner {
    /// The relay said "bad password".
    BadPassword,
    /// Anything else that is not `ports|||address`.
    Malformed,
}

/// The data ports in a relay's banner, `9010,9011|||203.0.113.9:4000`
/// (`ok` for none). The address after `|||` is where the relay saw us
/// come from, and is not used.
pub(super) fn parse_banner(plain: &[u8]) -> Result<Vec<u16>, Banner> {
    if plain == b"bad password" {
        return Err(Banner::BadPassword);
    }
    let text = std::str::from_utf8(plain).map_err(|_| Banner::Malformed)?;
    let (ports, address) = text.split_once("|||").ok_or(Banner::Malformed)?;
    if address.len() > 64 {
        return Err(Banner::Malformed);
    }
    if ports == "ok" {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for p in ports.split(',') {
        let port = (!p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse::<u16>().ok())
            .flatten()
            .filter(|p| *p > 0)
            .ok_or(Banner::Malformed)?;
        if out.len() >= MAX_BANNER_PORTS {
            return Err(Banner::Malformed);
        }
        out.push(port);
    }
    Ok(out)
}

#[cfg(test)]
pub(super) mod testing {
    //! A croc relay of our own for the tests, doing what croc's does
    //! (`tcp.go`'s `clientCommunication`): the PAKE's other side, the
    //! password check, the banner, rooms of two, a keepalive a second to a
    //! client waiting alone, and then copying bytes.

    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use tokio::io::AsyncWriteExt;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;

    use super::*;
    use crate::croc::conn::{PING, frame, read_frame};

    enum Room {
        /// The first is waiting for its partner's stream.
        Waiting(oneshot::Sender<TcpStream>),
        /// Two are in it.
        Full,
    }

    type Rooms = Arc<Mutex<HashMap<String, Room>>>;

    /// A running relay: a control port and data ports, all on loopback.
    pub(crate) struct TestRelay {
        pub(crate) port: u16,
        pub(crate) password: String,
        tasks: Vec<tokio::task::JoinHandle<()>>,
    }

    impl Drop for TestRelay {
        fn drop(&mut self) {
            for t in &self.tasks {
                t.abort();
            }
        }
    }

    impl TestRelay {
        pub(crate) fn relay(&self) -> Relay {
            Relay::at("127.0.0.1", self.port, &self.password)
        }
    }

    /// A relay with `data` data ports, and `password`.
    pub(crate) async fn start(password: &str, data: usize) -> TestRelay {
        let mut listeners = Vec::new();
        for _ in 0..=data {
            listeners.push(TcpListener::bind("127.0.0.1:0").await.unwrap());
        }
        let ports: Vec<u16> = listeners
            .iter()
            .map(|l| l.local_addr().unwrap().port())
            .collect();
        let banner = if data == 0 {
            "ok".to_owned()
        } else {
            ports[1..]
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        let mut tasks = Vec::new();
        for (i, listener) in listeners.into_iter().enumerate() {
            // Each port has rooms of its own, as croc's do.
            let rooms: Rooms = Arc::default();
            let banner = if i == 0 {
                banner.clone()
            } else {
                "ok".to_owned()
            };
            let password = password.to_owned();
            tasks.push(tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    tokio::spawn(client(
                        stream,
                        rooms.clone(),
                        banner.clone(),
                        password.clone(),
                    ));
                }
            }));
        }
        TestRelay {
            port: ports[0],
            password: password.to_owned(),
            tasks,
        }
    }

    async fn send(stream: &mut TcpStream, cipher: &Cipher, plain: &[u8]) -> bool {
        let sealed = cipher.seal(plain, random().unwrap()).unwrap();
        stream.write_all(&frame(&sealed)).await.is_ok()
    }

    async fn client(mut stream: TcpStream, rooms: Rooms, banner: String, password: String) {
        let Ok(theirs) = read_frame(&mut stream, 1 << 16).await else {
            return;
        };
        let Ok(pake) = Pake::answer(Curve::Siec, RELAY_PAKE_PASSWORD, random().unwrap(), &theirs)
        else {
            return;
        };
        if stream.write_all(&frame(&pake.message())).await.is_err() {
            return;
        }
        let Ok(salt) = read_frame(&mut stream, 1 << 16).await else {
            return;
        };
        let cipher = Cipher::new(&crypt::derive(&pake.key().unwrap(), &salt));
        let Some(pw) = read_frame(&mut stream, 1 << 16)
            .await
            .ok()
            .and_then(|f| cipher.open(&f))
        else {
            return;
        };
        if pw.trim_ascii() != password.as_bytes() {
            send(&mut stream, &cipher, b"bad password").await;
            return;
        }
        if !send(
            &mut stream,
            &cipher,
            format!("{banner}|||127.0.0.1:1").as_bytes(),
        )
        .await
        {
            return;
        }
        let Some(room) = read_frame(&mut stream, 1 << 16)
            .await
            .ok()
            .and_then(|f| cipher.open(&f))
        else {
            return;
        };
        let room = String::from_utf8(room).unwrap();
        let waiting = {
            let mut rooms = rooms.lock().unwrap();
            match rooms.remove(&room) {
                None => {
                    let (tx, rx) = oneshot::channel();
                    rooms.insert(room.clone(), Room::Waiting(tx));
                    Some(Err(rx))
                }
                Some(Room::Waiting(tx)) => {
                    rooms.insert(room.clone(), Room::Full);
                    Some(Ok(tx))
                }
                Some(Room::Full) => {
                    rooms.insert(room.clone(), Room::Full);
                    None
                }
            }
        };
        let Some(waiting) = waiting else {
            send(&mut stream, &cipher, b"room full").await;
            return;
        };
        if !send(&mut stream, &cipher, b"ok").await {
            return;
        }
        match waiting {
            // Second: hand the stream to the first, which copies.
            Ok(tx) => {
                let _ = tx.send(stream);
            }
            // First: a keepalive a second until the partner comes.
            Err(mut rx) => {
                let mut second = loop {
                    tokio::select! {
                        s = &mut rx => match s {
                            Ok(s) => break s,
                            Err(_) => return,
                        },
                        () = tokio::time::sleep(Duration::from_secs(1)) => {
                            if stream.write_all(&frame(PING)).await.is_err() {
                                rooms.lock().unwrap().remove(&room);
                                return;
                            }
                        }
                    }
                };
                let _ = tokio::io::copy_bidirectional(&mut stream, &mut second).await;
                rooms.lock().unwrap().remove(&room);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::field_reassign_with_default)] // Test scenes.
mod tests {
    use super::*;

    #[test]
    fn banners_are_ports_or_nothing() {
        assert_eq!(
            parse_banner(b"9010,9011,9012,9013|||1.2.3.4:5"),
            Ok(vec![9010, 9011, 9012, 9013])
        );
        assert_eq!(parse_banner(b"ok|||[::1]:5"), Ok(vec![]));
        assert_eq!(parse_banner(b"bad password"), Err(Banner::BadPassword));
        for bad in [
            &b"9010"[..],
            b"9010,|||x",
            b"0|||x",
            b"65536|||x",
            b"+9010|||x",
            b"9010 |||x",
            b"\xff|||x",
        ] {
            assert_eq!(parse_banner(bad), Err(Banner::Malformed), "{bad:?}");
        }
        let many = (1..=17)
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            parse_banner(format!("{many}|||x").as_bytes()),
            Err(Banner::Malformed)
        );
        let long = format!("9010|||{}", "a".repeat(65));
        assert_eq!(parse_banner(long.as_bytes()), Err(Banner::Malformed));
    }

    #[test]
    fn the_default_relay_is_croc_s() {
        let r = Relay::from_settings(&CrocSettings::default()).unwrap();
        assert_eq!((r.host.as_str(), r.port), (DEFAULT_HOST, DEFAULT_PORT));
        assert_eq!(r.password, DEFAULT_PASSWORD);
        let mut s = CrocSettings::default();
        s.relay = Some("[2001:db8::1]:9100".into());
        s.password = Some("hunter2".into());
        let r = Relay::from_settings(&s).unwrap();
        assert_eq!((r.host.as_str(), r.port), ("2001:db8::1", 9100));
        assert!(!format!("{r:?}").contains("hunter2"));
        s.relay = Some("tcp://x".into());
        assert_eq!(
            Relay::from_settings(&s).unwrap_err().code,
            ErrorCode::BadSettings
        );
    }

    #[tokio::test]
    async fn two_clients_meet_in_a_room() {
        let relay = testing::start("pass123", 4).await;
        let limit = Duration::from_secs(5);
        let r = relay.relay();
        let (a, b) = tokio::join!(join(&r, relay.port, "room", 1024, limit), async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            join(&r, relay.port, "room", 1024, limit).await
        });
        let (mut a, mut b) = (a.unwrap(), b.unwrap());
        assert_eq!(a.ports.len(), 4);
        assert_eq!(a.ports, b.ports);
        a.conn.send(b"hi", limit).await.unwrap();
        let deadline = tokio::time::Instant::now() + limit;
        assert_eq!(b.conn.recv_skipping(deadline).await.unwrap(), b"hi");
        b.conn.send(b"yo", limit).await.unwrap();
        assert_eq!(a.conn.recv_skipping(deadline).await.unwrap(), b"yo");
        // A third is turned away.
        let third = join(&r, relay.port, "room", 1024, limit).await;
        assert_eq!(third.unwrap_err().code, ErrorCode::BadCode);
        // A wrong password is said.
        let wrong = Relay::at("127.0.0.1", relay.port, "nope");
        let refused = join(&wrong, relay.port, "other", 1024, limit).await;
        assert_eq!(refused.unwrap_err().code, ErrorCode::BadSettings);
    }

    #[tokio::test]
    async fn a_client_alone_hears_keepalives() {
        let relay = testing::start("pass123", 0).await;
        let limit = Duration::from_secs(5);
        let r = relay.relay();
        let mut a = join(&r, relay.port, "lonely", 1024, limit).await.unwrap();
        assert!(a.ports.is_empty(), "ok: no data ports");
        assert_eq!(a.conn.recv(limit).await.unwrap(), conn_ping());
    }

    fn conn_ping() -> Vec<u8> {
        super::super::conn::PING.to_vec()
    }
}
