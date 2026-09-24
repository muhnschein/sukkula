//! Sukkula's trust boundary.
//!
//! Every value a peer can influence -- a file name, an alias, a size, a
//! message -- passes through this crate before anything else acts on it,
//! and this crate holds the only code in Sukkula that writes a file.
//!
//! It has no network code and no `unsafe`. The protocol adapters in
//! `sukkula-engine` translate wire formats into [`offer::RawOffer`]s and
//! byte streams; everything after that is decided here:
//!
//! | Rule | Module |
//! | --- | --- |
//! | S1 names | [`name`] |
//! | S2 display text | [`text`] |
//! | S3 writes | [`inbox`], [`store`] |
//! | S4 allocation, S6 limits | [`limits`], [`offer`] |
//! | S5 consent first | [`consent`] |
//! | S7 reach | [`reach`] |
//!
//! `docs/SECURITY.md` maps every rule to the tests that hold it.

#![forbid(unsafe_code)]

pub mod config;
pub mod consent;
pub mod hex;
pub mod inbox;
pub mod limits;
pub mod name;
pub mod offer;
pub mod reach;
pub mod store;
pub mod text;

pub use offer::Protocol;
