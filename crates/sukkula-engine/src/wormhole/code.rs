//! Wormhole codes: the one the user types (F-MW2), checked before anything
//! touches the network, and the one we allocate, as a QR code (F-MW1).

use magic_wormhole::Code;
use magic_wormhole::uri::WormholeTransferUri;
use qrcode::Color;
use url::Url;

use crate::api::{ErrorCode, ErrorInfo, QrCode};

/// Longest code accepted, after trimming.
pub(crate) const MAX_CODE_BYTES: usize = 128;

/// Most digits in a nameplate. Servers hand out small numbers; nine digits
/// is far beyond any real one and still fits a `u32`.
const MAX_NAMEPLATE_DIGITS: usize = 9;

/// Most words after the nameplate.
const MAX_WORDS: usize = 8;

/// Longest word. The PGP words the reference clients use are 4 to 11
/// letters; custom codes get room, not unlimited room.
const MAX_WORD_BYTES: usize = 32;

/// Shortest password the library accepts.
const MIN_PASSWORD_BYTES: usize = 4;

fn bad(message: &'static str) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::BadCode, message)
}

/// Checks a code the user typed and turns it into the library's [`Code`].
///
/// The accepted form is strict: a nameplate of ASCII digits with no leading
/// zero, then one to eight hyphen-separated words of ASCII letters and
/// digits, at most [`MAX_CODE_BYTES`] in all. Surrounding white space is
/// dropped and letters are lowercased, because phone keyboards capitalise
/// and every reference client generates lowercase words; a custom code with
/// capitals therefore cannot be typed here, which is the price of not
/// failing the common case. Anything else is refused before the network is
/// touched. The library then applies its own entropy check.
///
/// # Errors
///
/// [`ErrorCode::BadCode`].
pub(crate) fn parse(raw: &str) -> Result<Code, ErrorInfo> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(bad("the code is empty"));
    }
    if trimmed.len() > MAX_CODE_BYTES {
        return Err(bad("the code is too long"));
    }
    let code = trimmed.to_ascii_lowercase();
    let (nameplate, password) = code
        .split_once('-')
        .ok_or_else(|| bad("the code needs a number, a hyphen and words"))?;
    let nameplate_ok = !nameplate.is_empty()
        && nameplate.len() <= MAX_NAMEPLATE_DIGITS
        && nameplate.bytes().all(|b| b.is_ascii_digit())
        && !nameplate.starts_with('0');
    if !nameplate_ok {
        return Err(bad("the code must start with a number"));
    }
    let words_ok = password.len() >= MIN_PASSWORD_BYTES
        && password.split('-').count() <= MAX_WORDS
        && password.split('-').all(|w| {
            !w.is_empty()
                && w.len() <= MAX_WORD_BYTES
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        });
    if !words_ok {
        return Err(bad("the code's words are not usable"));
    }
    code.parse::<Code>()
        .map_err(|_| bad("the code is too easy to guess"))
}

/// The QR code the sender's screen shows (F-MW1). It encodes the
/// `wormhole-transfer:` URI (magic-wormhole's `uri` module), with the
/// mailbox server when it is not the default, so a scanning client can
/// find it; clients that do not know the scheme still show the code in it.
///
/// # Errors
///
/// [`ErrorCode::Internal`] if the URI does not fit a QR code, which a code
/// of ours never fails to.
pub(crate) fn qr(code: &Code, custom_mailbox: Option<&Url>) -> Result<QrCode, ErrorInfo> {
    let uri = WormholeTransferUri {
        code: code.clone(),
        rendezvous_server: custom_mailbox.cloned(),
        is_leader: false,
    }
    .to_string();
    qr_of(uri.as_bytes())
}

/// Rows of `0` and `1`, dark modules `1`.
fn qr_of(data: &[u8]) -> Result<QrCode, ErrorInfo> {
    let failed = || ErrorInfo::new(ErrorCode::Internal, "the QR code could not be made");
    let q = qrcode::QrCode::new(data).map_err(|_| failed())?;
    let width = q.width();
    if width == 0 {
        return Err(failed());
    }
    let rows: Vec<String> = q
        .to_colors()
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(|c| if *c == Color::Dark { '1' } else { '0' })
                .collect()
        })
        .collect();
    Ok(QrCode {
        size: u32::try_from(width).map_err(|_| failed())?,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn good_codes_parse() {
        assert_eq!(
            parse("7-guitarist-revenge").unwrap().to_string(),
            "7-guitarist-revenge"
        );
        assert_eq!(
            parse("  12-Guitarist-REVENGE \n").unwrap().to_string(),
            "12-guitarist-revenge"
        );
        assert!(parse("123456789-aardvark-adroitness-crucial").is_ok());
    }

    #[test]
    fn hostile_codes_are_refused_before_the_network() {
        for c in [
            "",
            "   ",
            "7",
            "7-",
            "-guitarist-revenge",
            "0-guitarist-revenge",
            "07-guitarist-revenge",
            "1234567890-guitarist-revenge",
            "x-guitarist-revenge",
            "7-guitarist--revenge",
            "7-guitarist-revenge-",
            "7-gui tarist-revenge",
            "7-guitarist-rev\u{202E}enge",
            "7-guitarist-revenge\0",
            "7-guitarist/../revenge",
            "7-ab",
            "7-a-b-c-d-e-f-g-h-i",
            "7-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-b",
            "7-güitarist-revenge",
            "٧-guitarist-revenge",
        ] {
            assert_eq!(parse(c).unwrap_err().code, ErrorCode::BadCode, "{c:?}");
        }
        let long = format!("7-{}", "abcd-".repeat(40));
        assert_eq!(parse(&long).unwrap_err().code, ErrorCode::BadCode);
    }

    #[test]
    fn the_qr_code_is_square_binary_and_starts_with_a_finder() {
        let code = parse("7-guitarist-revenge").unwrap();
        let q = qr(&code, None).unwrap();
        let size = usize::try_from(q.size).unwrap();
        assert!(size >= 21);
        assert_eq!(q.rows.len(), size);
        for row in &q.rows {
            assert_eq!(row.len(), size);
            assert!(row.bytes().all(|b| b == b'0' || b == b'1'));
        }
        // The top-left finder pattern: seven dark modules, then a light one.
        assert!(q.rows[0].starts_with("11111110"));
        assert!(q.rows[6].starts_with("11111110"));
        let custom: Url = "wss://mailbox.example/v1".parse().unwrap();
        let q2 = qr(&code, Some(&custom)).unwrap();
        assert!(q2.size > q.size, "the custom server is in the URI");
    }
}
