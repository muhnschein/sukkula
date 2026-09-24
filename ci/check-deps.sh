#!/bin/bash
# The dependency budget of spec §6, over Cargo.lock and over what the phone
# actually runs. clove's ci/check-net-deps.sh is the model: clippy.toml
# stops our code touching a thing, this stops a dependency smuggling the
# thing into the tree.
#
#     ci/check-deps.sh                  Cargo.lock, then sukkula-ffi's
#                                       aarch64 graph through cargo tree
#     ci/check-deps.sh --lock-only      Cargo.lock alone (no cargo needed)
#     ci/check-deps.sh --lock F --tree T --allow A
#                                       explicit inputs; T is saved
#                                       `cargo tree -f '{p}|{f}'` output
#                                       (ci/check-deps-selftest.sh)
#
# Refused outright, anywhere in the lockfile, dev-dependencies and other
# platforms included -- one of each is the whole budget (spec §6, §2):
#
#   - a second TLS stack beside rustls (OpenSSL above all: Harbour does not
#     let the package link it), or a crypto provider beside ring;
#   - a second D-Bus stack beside `dbus` over the system libdbus-1.so.3;
#   - libdbus-sys built `vendored` (Q7): a static libdbus the platform's
#     security updates never reach. Visible in the lockfile as libdbus-sys
#     depending on `cc`, which only that feature pulls;
#   - an async runtime beside tokio and the smol pieces magic-wormhole
#     brings.
#
# Refused unless ci/deps-allow.conf names them with a scope and a reason:
#
#   - Bluetooth stacks (bluer, btleplug): spec §2 reaches BlueZ over raw
#     D-Bus from our own code;
#   - process-spawning crates (S8), and tokio's `process` feature in the
#     shipped graph.
#
# A `dev` entry is additionally proved absent from sukkula-ffi's aarch64
# graph; an entry that matches nothing fails, like a stale waiver.
set -u

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
lock="$root/Cargo.lock"
allow="$root/ci/deps-allow.conf"
tree=""
lock_only=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --lock) lock=${2:?}; shift 2 ;;
        --tree) tree=${2:?}; shift 2 ;;
        --allow) allow=${2:?}; shift 2 ;;
        --lock-only) lock_only=1; shift ;;
        *) echo "usage: $0 [--lock-only] [--lock FILE] [--tree FILE] [--allow FILE]" >&2; exit 2 ;;
    esac
done

[[ -f "$lock" ]] || { echo "check-deps: FAIL $lock not found" >&2; exit 1; }
[[ -f "$allow" ]] || { echo "check-deps: FAIL $allow not found" >&2; exit 1; }

status=0
bad() { echo "check-deps: FAIL $*" >&2; status=1; return 0; }

TLS='openssl openssl-sys openssl-src native-tls tokio-native-tls hyper-tls async-native-tls
     boring boring-sys tokio-boring hyper-boring mbedtls mbedtls-sys-auto wolfssl wolfssl-sys
     gnutls gnutls-sys s2n-tls s2n-tls-sys rustls-openssl
     aws-lc-rs aws-lc-sys aws-lc-fips-sys'
DBUS='zbus zbus_macros zbus_names zvariant zvariant_derive rustbus'
RUNTIMES='async-std async-global-executor glommio monoio actix-rt tokio-uring compio'
BLUETOOTH='bluer btleplug bluez-async bluez-generated'
SPAWN='async-process duct subprocess command-group process-wrap shared_child rusty-fork wait-timeout'
# Platform TLS and keychain bindings: locked for other targets, and never
# to reach the aarch64 graph.
FOREIGN='security-framework security-framework-sys schannel core-foundation'

# Crate names in the lockfile, and each package's own dependency list.
names=$(sed -n 's/^name = "\(.*\)"$/\1/p' "$lock" | sort -u)
in_lock() { grep -qx -- "$1" <<< "$names"; }

# The allow-list: name, scope, reason.
declare -A allow_scope=()
declare -A allow_used=()
while IFS= read -r line; do
    line=${line%%#*}
    read -r entry scope reason <<< "$line"
    [[ -n "${entry:-}" ]] || continue
    case "${scope:-}" in
        dev|ship) ;;
        *) bad "ci/deps-allow.conf: '$entry' has scope '${scope:-}', not dev or ship"; continue ;;
    esac
    [[ -n "${reason:-}" ]] || { bad "ci/deps-allow.conf: '$entry' gives no reason"; continue; }
    allow_scope[$entry]=$scope
done < "$allow"

# Hard refusals have no allow entry to write.
for entry in "${!allow_scope[@]}"; do
    crate=${entry%%/*}
    for hard in $TLS $DBUS $RUNTIMES; do
        [[ "$crate" = "$hard" ]] && bad "ci/deps-allow.conf lists '$crate', which is refused outright"
    done
done

for crate in $TLS; do
    in_lock "$crate" && bad "'$crate' is in Cargo.lock: one TLS stack and one crypto provider, rustls over ring (spec §6)"
done
for crate in $DBUS; do
    in_lock "$crate" && bad "'$crate' is in Cargo.lock: one D-Bus stack, dbus over the system libdbus-1 (spec §2)"
done
for crate in $RUNTIMES; do
    in_lock "$crate" && bad "'$crate' is in Cargo.lock: no async runtime beyond tokio and magic-wormhole's smol pieces (spec §6)"
done

# libdbus-sys `vendored`: the feature's only effect on the graph is its
# optional `cc` build-dependency, so the lockfile shows it.
if awk '
    /^\[\[package\]\]/ { pkg = "" ; deps = 0 }
    /^name = "libdbus-sys"$/ { pkg = "libdbus-sys" }
    pkg == "libdbus-sys" && /^dependencies = \[/ { deps = 1; next }
    pkg == "libdbus-sys" && deps && /^\]/ { deps = 0 }
    pkg == "libdbus-sys" && deps && /"cc( |")/ { found = 1 }
    END { exit found ? 0 : 1 }' "$lock"; then
    bad "libdbus-sys is built 'vendored' (it depends on cc): a static libdbus instead of the system's (Q7)"
fi

for crate in $BLUETOOTH $SPAWN; do
    in_lock "$crate" || continue
    if [[ -n "${allow_scope[$crate]:-}" ]]; then
        allow_used[$crate]=1
        echo "check-deps: allowed $crate (${allow_scope[$crate]})"
    else
        bad "'$crate' is in Cargo.lock and not in ci/deps-allow.conf: Bluetooth stacks and process-spawning crates need a reviewed reason (spec §2, S8)"
    fi
done

# -- the shipped graph --------------------------------------------------------
if [[ "$lock_only" = 0 ]]; then
    if [[ -z "$tree" ]]; then
        command -v cargo >/dev/null 2>&1 || { bad "cargo not found; the shipped graph cannot be checked (--lock-only skips it knowingly)"; exit 1; }
        tree=$(mktemp)
        trap 'rm -f "$tree"' EXIT
        if ! (cd "$root" && cargo tree --locked -p sukkula-ffi --target aarch64-unknown-linux-gnu \
                -e normal,build --prefix none -f '{p}|{f}' > "$tree" 2>/dev/null); then
            bad "cargo tree failed for sukkula-ffi on aarch64"
            exit 1
        fi
    fi
    [[ -s "$tree" ]] || { bad "the shipped graph is empty"; exit 1; }

    shipped=$(sed 's/ .*//' "$tree" | sort -u)
    in_ship() { grep -qx -- "$1" <<< "$shipped"; }
    features_of() { grep -E "^$1 " "$tree" | head -1 | sed 's/^[^|]*|//' | tr ',' '\n'; }

    for crate in $TLS $DBUS $RUNTIMES $FOREIGN; do
        in_ship "$crate" && bad "'$crate' is in the phone build (sukkula-ffi, aarch64)"
    done
    for crate in $BLUETOOTH $SPAWN; do
        in_ship "$crate" || continue
        case "${allow_scope[$crate]:-}" in
            ship) ;;
            dev) bad "'$crate' is allowed for tests only (dev), but it is in the phone build" ;;
            *) bad "'$crate' is in the phone build and not in ci/deps-allow.conf" ;;
        esac
    done
    if features_of tokio | grep -qx process; then
        if [[ "${allow_scope[tokio/process]:-}" = ship ]]; then
            allow_used[tokio/process]=1
            echo "check-deps: allowed tokio/process (ship)"
        else
            bad "tokio's 'process' feature is in the phone build and not in ci/deps-allow.conf (S8)"
        fi
    fi
    if features_of libdbus-sys | grep -qx vendored || features_of dbus | grep -qx vendored; then
        bad "dbus/libdbus-sys 'vendored' is enabled in the phone build (Q7)"
    fi
    echo "check-deps: shipped graph checked ($(wc -l <<< "$shipped") crates)"
else
    # Without the graph a `ship`-scoped feature entry cannot be judged;
    # it is not stale, only unexamined.
    for entry in "${!allow_scope[@]}"; do
        [[ "$entry" == */* ]] && allow_used[$entry]=1
    done
fi

for entry in "${!allow_scope[@]}"; do
    [[ -n "${allow_used[$entry]:-}" ]] ||
        bad "stale entry '$entry' in ci/deps-allow.conf matches nothing; delete it"
done

[[ "$status" -eq 0 ]] && echo "check-deps: ok"
exit "$status"
