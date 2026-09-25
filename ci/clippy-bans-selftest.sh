#!/bin/bash
# Prove that clippy.toml's bans fire on what they were added for.
#
# A ban is a path string clippy has to resolve, and a path that resolves
# to nothing -- a typo, a trait method (`OpenOptions::default` is
# `Default::default`), an item behind a feature -- bans nothing, silently
# under `allow-invalid`. So a throwaway crate, with the repository's
# clippy.toml and the versions of tokio, rustix and dbus Cargo.lock ships,
# calls each API one of these bans exists for, and every call has to come
# back as a disallowed method by the path it is banned under; and the
# calls the bans must leave alone -- the explicit-address D-Bus
# constructors the engine uses, a read-only open, the effective uid --
# must come back clean.
#
# The cases are the holes finding "clippy.toml's S3 'one writer' ban
# misses tokio OpenOptions::default and Unix socket binds; rustix twins of
# banned process-state calls are unbanned" and the D-Bus half of "S8 gate
# does not cover libdbus" found. Needs the workspace's dependencies
# fetched (any cargo build of it has), since it runs offline, and
# libdbus-1-dev, as the workspace does.
set -u

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
crate="$work/bans"
mkdir -p "$crate/src"
cp "$root/clippy.toml" "$root/rust-toolchain.toml" "$root/Cargo.lock" "$crate/"

cat > "$crate/Cargo.toml" <<'EOF'
[package]
name = "clippy-bans-selftest"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
tokio = { version = "1", default-features = false, features = ["fs", "net", "rt"] }
rustix = { version = "1", default-features = false, features = ["std", "fs", "process", "net"] }
dbus = "0.9"

[lints.clippy]
disallowed_methods = "warn"
disallowed_types = "warn"

[workspace]
EOF

# Each function breaks one rule; the path it must be reported under is on
# the line above it.
cat > "$crate/src/lib.rs" <<'EOF'
#![allow(dead_code, unused_must_use)]
mod allowed;
use std::path::Path;

// BAN tokio::fs::OpenOptions::open
async fn tokio_default(p: &Path) -> std::io::Result<tokio::fs::File> {
    tokio::fs::OpenOptions::default().write(true).create(true).truncate(true).open(p).await
}
// BAN tokio::fs::OpenOptions::open
async fn tokio_default_trait(p: &Path) -> std::io::Result<tokio::fs::File> {
    let mut o: tokio::fs::OpenOptions = Default::default();
    o.write(true).create(true).open(p).await
}
// BAN tokio::fs::OpenOptions::open
async fn tokio_from_std(p: &Path, s: &std::fs::OpenOptions) -> std::io::Result<tokio::fs::File> {
    tokio::fs::OpenOptions::from(s.clone()).open(p).await
}
// BAN std::fs::OpenOptions::open
fn std_clone(p: &Path, s: &std::fs::OpenOptions) -> std::io::Result<std::fs::File> {
    s.clone().open(p)
}
// BAN std::os::unix::net::UnixListener::bind_addr
fn std_listener(a: &std::os::unix::net::SocketAddr) -> std::io::Result<std::os::unix::net::UnixListener> {
    std::os::unix::net::UnixListener::bind_addr(a)
}
// BAN std::os::unix::net::UnixDatagram::bind_addr
fn std_datagram(a: &std::os::unix::net::SocketAddr) -> std::io::Result<std::os::unix::net::UnixDatagram> {
    std::os::unix::net::UnixDatagram::bind_addr(a)
}
// BAN tokio::net::UnixSocket::bind
fn tokio_socket(s: &tokio::net::UnixSocket, p: &Path) -> std::io::Result<()> {
    s.bind(p)
}
// BAN rustix::net::bind
fn rustix_bind(fd: &std::os::fd::OwnedFd, a: &rustix::net::SocketAddrUnix) -> rustix::io::Result<()> {
    rustix::net::bind(fd, a)
}
// BAN rustix::process::chdir
fn rustix_chdir(p: &Path) -> rustix::io::Result<()> {
    rustix::process::chdir(p)
}
// BAN rustix::process::fchdir
fn rustix_fchdir(fd: &std::fs::File) -> rustix::io::Result<()> {
    rustix::process::fchdir(fd)
}
// BAN rustix::fs::Dir::chdir
fn rustix_dir_chdir(d: &rustix::fs::Dir) -> rustix::io::Result<()> {
    d.chdir()
}
// BAN rustix::process::chroot
fn rustix_chroot(p: &Path) -> rustix::io::Result<()> {
    rustix::process::chroot(p)
}
// BAN rustix::process::umask
fn rustix_umask() -> rustix::fs::Mode {
    rustix::process::umask(rustix::fs::Mode::empty())
}
// BAN dbus::blocking::Connection::new_session
fn dbus_1() -> Result<dbus::blocking::Connection, dbus::Error> { dbus::blocking::Connection::new_session() }
// BAN dbus::blocking::Connection::new_system
fn dbus_2() -> Result<dbus::blocking::Connection, dbus::Error> { dbus::blocking::Connection::new_system() }
// BAN dbus::blocking::LocalConnection::new_session
fn dbus_3() -> Result<dbus::blocking::LocalConnection, dbus::Error> { dbus::blocking::LocalConnection::new_session() }
// BAN dbus::blocking::LocalConnection::new_system
fn dbus_4() -> Result<dbus::blocking::LocalConnection, dbus::Error> { dbus::blocking::LocalConnection::new_system() }
// BAN dbus::blocking::SyncConnection::new_session
fn dbus_5() -> Result<dbus::blocking::SyncConnection, dbus::Error> { dbus::blocking::SyncConnection::new_session() }
// BAN dbus::blocking::SyncConnection::new_system
fn dbus_6() -> Result<dbus::blocking::SyncConnection, dbus::Error> { dbus::blocking::SyncConnection::new_system() }
// BAN dbus::channel::Channel::get_private
fn dbus_7() -> Result<dbus::channel::Channel, dbus::Error> { dbus::channel::Channel::get_private(dbus::channel::BusType::Session) }
// BAN dbus::ffidisp::Connection::new_session
fn dbus_8() -> Result<dbus::ffidisp::Connection, dbus::Error> { dbus::ffidisp::Connection::new_session() }
// BAN dbus::ffidisp::Connection::new_system
fn dbus_9() -> Result<dbus::ffidisp::Connection, dbus::Error> { dbus::ffidisp::Connection::new_system() }
// BAN dbus::ffidisp::Connection::get_private
fn dbus_10() -> Result<dbus::ffidisp::Connection, dbus::Error> { dbus::ffidisp::Connection::get_private(dbus::ffidisp::BusType::System) }
EOF

# What the bans must not touch: how the engine opens D-Bus, reads a file
# and asks who it runs as.
cat > "$crate/src/allowed.rs" <<'EOF'
use std::path::Path;

fn explicit_channel(address: &str) -> Result<dbus::channel::Channel, dbus::Error> {
    dbus::channel::Channel::open_private(address)
}
fn explicit_blocking(address: &str) -> Result<dbus::blocking::Connection, dbus::Error> {
    dbus::blocking::Connection::new_address(address)
}
fn read(p: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(p)
}
async fn read_async(p: &Path) -> std::io::Result<tokio::fs::File> {
    tokio::fs::File::open(p).await
}
fn uid() -> u32 {
    rustix::process::geteuid().as_raw()
}
EOF

out="$work/clippy.log"
if ! (cd "$crate" && CARGO_TARGET_DIR="$root/target/clippy-bans-selftest" \
        cargo clippy --offline --quiet --message-format=short 2> "$out"); then
    if grep -q '^error' "$out" && ! grep -q 'disallowed' "$out"; then
        echo "clippy-bans: FAIL the probe crate does not build:" >&2
        cat "$out" >&2
        exit 1
    fi
fi

status=0
cases=0
# Every BAN line's function has to be reported by that path, on its line.
while IFS=: read -r line path; do
    cases=$((cases + 1))
    fn_line=$((line + 1))
    body_end=$(awk -v s="$fn_line" 'NR > s && /^(\/\/|})/ { print NR; exit }' "$crate/src/lib.rs")
    body_end=${body_end:-$fn_line}
    if awk -F: -v s="$fn_line" -v e="$body_end" -v p="$path" '
        $1 == "src/lib.rs" && $2 >= s && $2 <= e && index($0, "disallowed method `" p "`") { found = 1 }
        END { exit !found }' "$out"; then
        echo "clippy-bans: ok   $path (src/lib.rs:$fn_line)"
    else
        echo "clippy-bans: FAIL src/lib.rs:$fn_line should be reported as a disallowed method \`$path\`" >&2
        status=1
    fi
done < <(sed -n 's|^// BAN \(.*\)$|\1|p' "$crate/src/lib.rs" | paste -d: <(grep -n '^// BAN ' "$crate/src/lib.rs" | cut -d: -f1) -)

cases=$((cases + 1))
if grep -q '^src/allowed.rs' "$out"; then
    echo "clippy-bans: FAIL a ban fires on what it must leave alone:" >&2
    grep '^src/allowed.rs' "$out" >&2
    status=1
else
    echo "clippy-bans: ok   the explicit-address D-Bus constructors, a read-only open, geteuid: untouched"
fi

# Nothing in clippy.toml that this crate can see may be unresolvable.
cases=$((cases + 1))
if grep -q 'does not refer to' "$out"; then
    echo "clippy-bans: FAIL clippy.toml names something that is not there:" >&2
    grep -A2 'does not refer to' "$out" >&2
    status=1
else
    echo "clippy-bans: ok   every path clippy.toml names resolves here"
fi

echo
if [[ "$status" -eq 0 ]]; then
    echo "clippy-bans: ok ($cases cases)"
else
    echo "clippy-bans: FAILED" >&2
fi
exit "$status"
