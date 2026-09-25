//! Quick Share from the first byte: the plaintext handshake a stranger on
//! the LAN reaches before anything is authenticated -- the frame reader's
//! limits, the connection request and the endpoint info inside it (the
//! sender's name), UKEY2's client init and finish, the peer's P-256 key --
//! and then, when the input completes the exchange, the encrypted channel
//! under the keys it derived (spec §7 "Quick Share frames").
//!
//! Two kinds of input. `Stream` is bytes on the socket as they are, length
//! prefixes included. `Scripted` is a sender that follows the protocol
//! wherever the input does not say otherwise: it commits to the client
//! finish it will really send, so the commitment check passes and the
//! peer's key -- a real one with its coordinates encoded every way Java's
//! `BigInteger` or a careless sender could, or any bytes -- reaches
//! `decode_p256_point` and the key derivation.
//!
//! Asserted: no event of any kind before the key exchange; the name the
//! connection request carried is the one handed over, and the consent
//! dialog shows it S2-clean; a finished exchange yields a four-digit PIN;
//! and after it everything `quickshare_frame` asserts.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use sukkula_fuzz::handshake::{Input, run};

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = Input::arbitrary(&mut Unstructured::new(data)) {
        run(&input);
    }
});
