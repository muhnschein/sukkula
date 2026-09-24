//! S6: the hard limits. Every adapter enforces these through
//! [`crate::offer::Offer::validate`] and [`crate::inbox`]; none of them is a
//! setting.

use std::time::Duration;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

/// Largest single file accepted.
pub const MAX_FILE_BYTES: u64 = 8 * GIB;

/// Largest sum of file sizes in one offer.
pub const MAX_OFFER_BYTES: u64 = 16 * GIB;

/// Most files in one offer.
pub const MAX_FILES_PER_OFFER: usize = 500;

/// Largest text message, and largest JSON message read from a peer or
/// handed to the engine as a command.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Longest file name, in bytes, after sanitising (S1).
pub const MAX_NAME_BYTES: usize = 200;

/// Longest extension kept intact when a name is shortened (S1).
pub const MAX_EXTENSION_BYTES: usize = 16;

/// Longest alias or device name shown in the UI, in characters (S2).
pub const MAX_ALIAS_CHARS: usize = 64;

/// Longest device model shown in the UI, in characters (S2).
pub const MAX_MODEL_CHARS: usize = 64;

/// Longest MIME type kept from an offer, in bytes.
pub const MAX_MIME_BYTES: usize = 127;

/// Longest PIN accepted in an offer or in the settings.
pub const MAX_PIN_CHARS: usize = 16;

/// Most combining marks kept after one base character (S2). Stacks of them
/// draw over neighbouring lines.
pub const MAX_COMBINING_RUN: usize = 3;

/// How long an offer waits for the user before it is declined (F-C3).
pub const OFFER_TIMEOUT: Duration = Duration::from_secs(60);

/// How many offers may wait for the user at once; the next is declined
/// without any UI (F-C3).
pub const MAX_PENDING_OFFERS: usize = 2;

/// Upper bound on any single network read or write making no progress
/// (S6). Adapters wrap every read in a timeout no longer than this.
pub const NETWORK_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Upper bound on a protocol handshake, from connect to a parsed offer.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// Chunk size adapters should read and write with. Bounded buffers are what
/// keeps a hostile size field from becoming an allocation (S4).
pub const IO_CHUNK_BYTES: usize = 64 * 1024;

/// Discovery replies or registrations answered per peer IP per window (S7).
pub const DISCOVERY_BURST: u32 = 8;

/// The window [`DISCOVERY_BURST`] refills over.
pub const DISCOVERY_WINDOW: Duration = Duration::from_secs(10);

/// Incoming offers accepted for the consent queue per peer IP per window.
/// Without it one peer can keep both consent slots busy forever.
pub const OFFER_BURST: u32 = 3;

/// The window [`OFFER_BURST`] refills over.
pub const OFFER_WINDOW: Duration = Duration::from_secs(60);

/// Incoming offers accepted for the consent queue per minute from all peers
/// together. The per-IP limit alone is no limit against a peer that rotates
/// its source address, which on IPv6 costs it nothing.
pub const GLOBAL_OFFER_BURST: u32 = 10;

/// The window [`GLOBAL_OFFER_BURST`] refills over.
pub const GLOBAL_OFFER_WINDOW: Duration = Duration::from_secs(60);

/// Distinct peer IPs a rate limiter remembers. Beyond this the stalest
/// entry is forgotten, so a spoofed-source flood costs bounded memory.
pub const RATE_LIMIT_ENTRIES: usize = 256;

/// Most peers kept in a discovery list at once.
pub const MAX_PEERS: usize = 64;

/// Most transfers running at once, in either direction.
pub const MAX_ACTIVE_TRANSFERS: usize = 8;

/// Most bytes of an engine event, as JSON. Events are built by us, from
/// sanitised values, and are bounded by construction; this is the check
/// that the construction is right.
pub const MAX_EVENT_BYTES: usize = 256 * 1024;
