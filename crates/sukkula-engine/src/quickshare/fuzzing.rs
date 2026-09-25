//! Entry points for the fuzz targets in `fuzz/` (spec §7 "Parsers"): the
//! adapter's conversions of what rqs_lib parsed from a peer, reachable
//! without a socket. The frames themselves are rqs_lib's to parse, and the
//! targets drive its public [`rqs_lib::InboundRequest`] directly; these are
//! thin wrappers over the adapter's own code, which the engine does not
//! call through here.

use rqs_lib::InboundRequest;
use rqs_lib::hdl::info::Introduction;
use rqs_lib::utils::{parse_mdns_endpoint_info, parse_mdns_name};
use sukkula_core::offer::RawOffer;
use sukkula_core::text;
use tokio::io::{AsyncRead, AsyncWrite};

use super::{discovery, receive};
use crate::api::Peer;

/// The raw offer the adapter asks the user about for `introduction`, with
/// the sender's name and the PIN from `ir`.
pub fn raw_offer<S: AsyncRead + AsyncWrite + Unpin>(
    ir: &InboundRequest<S>,
    introduction: &Introduction,
) -> RawOffer {
    receive::raw_offer(ir, introduction)
}

/// A received text as the adapter reports it (F-C4).
#[must_use]
pub fn text_received(text: &str) -> String {
    text::message(text)
}

/// The peer the UI lists for a resolved mDNS service: rqs_lib's parsers of
/// the instance name and the `n` record, as its browser applies them, then
/// the adapter's naming. `None` where either skips the service.
#[must_use]
pub fn peer_from_mdns(fullname: &str, n: &str) -> Option<Peer> {
    let (device_type, name) = parse_mdns_endpoint_info(n).ok()?;
    let endpoint_id = parse_mdns_name(fullname)?;
    Some(discovery::listed(endpoint_id, &name, device_type))
}
