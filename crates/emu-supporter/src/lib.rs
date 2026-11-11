//! Supporter keys ([decision 0017](../../../docs/decisions/0017-supporter-keys.md)).
//!
//! Imago Emulator is free, and everything that plays a game is in the one
//! build everyone downloads. Someone who pays to support it gets a key by
//! email, and the key turns on a few cosmetic choices as thanks. This crate
//! reads keys and, for whoever holds the signing key, writes them.
//!
//! A key is checked on the computer it is pasted into, against a public
//! key built into the app: no account, no activation and no request to any
//! server, so a key works offline and for as long as the app does.
//!
//! As text a key is [`PREFIX`] and then, in URL-safe base64 without
//! padding, these bytes:
//!
//! | Bytes | What |
//! |---|---|
//! | 1 | The format's version, [`VERSION`] |
//! | 8 | The key's number, big-endian: what [`REVOKED`] names |
//! | 4 | The day it was issued, in days since 1970-01-01, big-endian |
//! | 0 to [`MAX_NAME`] | The supporter's name, UTF-8; may be empty |
//! | 64 | An Ed25519 signature of [`CONTEXT`] and everything before it |
//!
//! Plain Ed25519 over those bytes, so that whatever sends keys out (a
//! Worker with WebCrypto, say) can sign them without this crate.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_compact::{KeyPair, PublicKey, Seed, Signature};

/// What every key starts with.
pub const PREFIX: &str = "IMAGO-";

/// The one version of the format there is.
pub const VERSION: u8 = 1;

/// The longest name a key holds, in bytes of UTF-8.
pub const MAX_NAME: usize = 64;

/// What is signed before a key's own bytes, so that a signature made for a
/// key is never one for anything else the signing key may come to sign.
pub const CONTEXT: &[u8] = b"imago-emulator supporter key\0";

/// The bytes before the name: the version, the number and the day.
const HEADER: usize = 1 + 8 + 4;

/// The public halves of the signing keys the app trusts. A key signed by
/// any of them is good, so a signing key can be replaced without ending
/// the keys already sent out: add the new one here and sign with it.
///
/// The private half is not in this repository. `supporter-key keygen`
/// made it, in a file only its owner reads (see the tool's help).
pub const PUBLIC_KEYS: [[u8; 32]; 1] = [[
    0x88, 0x07, 0x49, 0x84, 0x78, 0x20, 0x32, 0x4C, 0x9B, 0xA0, 0x42, 0x9F, 0xA6, 0xC0, 0x9B, 0x09,
    0x82, 0x15, 0x6B, 0xDE, 0xDE, 0x8A, 0x92, 0x02, 0x30, 0xEC, 0x59, 0x92, 0x76, 0x78, 0x30, 0x36,
]];

/// The numbers of keys that no longer count: refunded, or published. A
/// key is checked offline, so taking one back takes a new build.
pub const REVOKED: [u64; 0] = [];

/// What a good key says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supporter {
    /// The key's number.
    pub id: u64,
    /// The day it was issued, in days since 1970-01-01.
    pub issued: u32,
    /// The supporter's name, to thank them by; empty if the key has none.
    pub name: String,
}

/// Why some text is not a key that counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// It does not start with [`PREFIX`]: something else was pasted.
    #[error("this is not a supporter key")]
    NotAKey,
    /// It starts as a key does, but part of it is missing or changed.
    #[error("part of this supporter key is missing")]
    Damaged,
    /// It is whole, but no trusted signing key signed it.
    #[error("this supporter key was not issued for Imago Emulator")]
    Forged,
    /// It is signed, in a format this build does not know.
    #[error("this supporter key needs a newer Imago Emulator")]
    Newer,
    /// It was taken back.
    #[error("this supporter key is no longer valid")]
    Revoked,
}

/// What `text` says, if it is a key the app trusts: one signed by any of
/// [`PUBLIC_KEYS`] and not among the [`REVOKED`]. Spaces and line breaks
/// in it, such as an email adds, are ignored.
///
/// # Errors
///
/// Returns [`KeyError`] saying why it is not.
pub fn read(text: &str) -> Result<Supporter, KeyError> {
    let supporter = read_with(&PUBLIC_KEYS, text)?;
    if REVOKED.contains(&supporter.id) {
        return Err(KeyError::Revoked);
    }
    Ok(supporter)
}

/// What `text` says, if it is a key signed by any of `trusted`.
///
/// # Errors
///
/// Returns [`KeyError`] saying why it is not.
pub fn read_with(trusted: &[[u8; 32]], text: &str) -> Result<Supporter, KeyError> {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let body = text.strip_prefix(PREFIX).ok_or(KeyError::NotAKey)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(body)
        .map_err(|_| KeyError::Damaged)?;
    let signed = bytes
        .len()
        .checked_sub(Signature::BYTES)
        .filter(|&signed| (HEADER..=HEADER + MAX_NAME).contains(&signed))
        .ok_or(KeyError::Damaged)?;
    let (payload, signature) = bytes.split_at(signed);
    let signature = Signature::from_slice(signature).map_err(|_| KeyError::Damaged)?;
    let message = [CONTEXT, payload].concat();
    let genuine = trusted.iter().any(|key| {
        PublicKey::from_slice(key).is_ok_and(|key| key.verify(&message, &signature).is_ok())
    });
    if !genuine {
        return Err(KeyError::Forged);
    }
    // Signed, so what follows is what the signer wrote.
    if payload[0] != VERSION {
        return Err(KeyError::Newer);
    }
    let number = |range: std::ops::Range<usize>| payload.get(range).ok_or(KeyError::Damaged);
    let id = u64::from_be_bytes(number(1..9)?.try_into().map_err(|_| KeyError::Damaged)?);
    let issued = u32::from_be_bytes(number(9..13)?.try_into().map_err(|_| KeyError::Damaged)?);
    let name = std::str::from_utf8(&payload[HEADER..]).map_err(|_| KeyError::Damaged)?;
    Ok(Supporter {
        id,
        issued,
        name: name.to_owned(),
    })
}

/// Why a key could not be issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IssueError {
    /// The name is longer than [`MAX_NAME`] bytes.
    #[error("the name is longer than {MAX_NAME} bytes")]
    NameTooLong,
    /// The name has a line break or another control character in it.
    #[error("the name has a control character in it")]
    NameNotPlain,
}

/// A key numbered `id`, issued on day `issued` to `name` (which may be
/// empty), signed with the signing key whose seed is `seed`.
///
/// # Errors
///
/// Returns [`IssueError`] if the name does not fit a key.
pub fn issue(seed: &[u8; 32], id: u64, issued: u32, name: &str) -> Result<String, IssueError> {
    let name = name.trim();
    if name.len() > MAX_NAME {
        return Err(IssueError::NameTooLong);
    }
    if name.chars().any(char::is_control) {
        return Err(IssueError::NameNotPlain);
    }
    let mut bytes = vec![VERSION];
    bytes.extend(id.to_be_bytes());
    bytes.extend(issued.to_be_bytes());
    bytes.extend(name.as_bytes());
    let message = [CONTEXT, &bytes].concat();
    // No noise: one key's text is the same however often it is issued.
    let signature = KeyPair::from_seed(Seed::new(*seed)).sk.sign(message, None);
    bytes.extend(signature.as_ref());
    Ok(format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)))
}

/// The public half of the signing key whose seed is `seed`: what
/// [`PUBLIC_KEYS`] lists.
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    *KeyPair::from_seed(Seed::new(*seed)).pk
}

/// A new signing key's seed, from the system's random numbers.
pub fn new_seed() -> [u8; 32] {
    *Seed::generate()
}

/// A number for a new key, from the system's random numbers: no two keys
/// share one, and none says how many were issued before it.
pub fn new_id() -> u64 {
    let random = new_seed();
    let mut id = [0; 8];
    id.copy_from_slice(&random[..8]);
    u64::from_be_bytes(id)
}

/// The date of day `days` since 1970-01-01, as a year, month and day.
pub fn date(days: u32) -> (i64, u32, u32) {
    // Howard Hinnant's `civil_from_days`: eras of 400 years, counted from
    // a 1 March, so that a leap day ends its year.
    let z = i64::from(days) + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = (month_from_march + 2) % 12 + 1;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    // A month is 1 to 12 and a day 1 to 31.
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A signing key for the tests alone.
    const SEED: [u8; 32] = [7; 32];

    fn trusted() -> [[u8; 32]; 1] {
        [public_key(&SEED)]
    }

    #[test]
    fn a_key_says_who_it_was_issued_to_and_when() {
        let key = issue(&SEED, 0x0123_4567_89AB_CDEF, 20_768, "Ada Lovelace").expect("issued");
        assert!(key.starts_with(PREFIX), "{key}");
        assert_eq!(
            read_with(&trusted(), &key),
            Ok(Supporter {
                id: 0x0123_4567_89AB_CDEF,
                issued: 20_768,
                name: "Ada Lovelace".to_owned(),
            })
        );
        // The same key again is the same text.
        assert_eq!(
            issue(&SEED, 0x0123_4567_89AB_CDEF, 20_768, "Ada Lovelace"),
            Ok(key)
        );
    }

    #[test]
    fn a_key_needs_no_name_and_holds_one_in_any_script() {
        let plain = issue(&SEED, 1, 2, "").expect("issued");
        assert_eq!(read_with(&trusted(), &plain).expect("read").name, "");
        // The shortest key there is: 13 bytes and a signature.
        assert_eq!(plain.len(), PREFIX.len() + (77_usize * 4).div_ceil(3));
        let named = issue(&SEED, 1, 2, "  ポケット 🎮 ").expect("issued");
        assert_eq!(
            read_with(&trusted(), &named).expect("read").name,
            "ポケット 🎮"
        );
        assert_eq!(
            issue(&SEED, 1, 2, &"é".repeat(33)),
            Err(IssueError::NameTooLong)
        );
        assert!(issue(&SEED, 1, 2, &"é".repeat(32)).is_ok());
        assert_eq!(
            issue(&SEED, 1, 2, "two\nlines"),
            Err(IssueError::NameNotPlain)
        );
    }

    #[test]
    fn a_key_reads_however_an_email_wrapped_it() {
        let key = issue(&SEED, 9, 9, "Wrapped").expect("issued");
        let (start, end) = key.split_at(40);
        let wrapped = format!("  {start}\r\n  {end} \n");
        assert_eq!(
            read_with(&trusted(), &wrapped).expect("read").name,
            "Wrapped"
        );
    }

    #[test]
    fn a_key_changed_anywhere_is_refused() {
        let key = issue(&SEED, 42, 20_000, "Mallory").expect("issued");
        let body = &key[PREFIX.len()..];
        let bytes = URL_SAFE_NO_PAD.decode(body).expect("base64");
        // Every byte, the name's and the signature's alike.
        for at in 0..bytes.len() {
            let mut changed = bytes.clone();
            changed[at] ^= 1;
            let text = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(changed));
            assert_eq!(
                read_with(&trusted(), &text),
                Err(KeyError::Forged),
                "byte {at}"
            );
        }
        // A name made longer, or cut short.
        let mut longer = bytes.clone();
        longer.insert(HEADER, b'X');
        let text = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(longer));
        assert_eq!(read_with(&trusted(), &text), Err(KeyError::Forged));
    }

    #[test]
    fn a_key_signed_by_someone_else_is_refused() {
        let key = issue(&[8; 32], 42, 20_000, "Mallory").expect("issued");
        assert_eq!(read_with(&trusted(), &key), Err(KeyError::Forged));
        // Either of two trusted signing keys will do.
        let both = [public_key(&SEED), public_key(&[8; 32])];
        assert!(read_with(&both, &key).is_ok());
        assert_eq!(read_with(&[], &key), Err(KeyError::Forged));
        // A public key that is no point on the curve trusts nothing.
        assert_eq!(read_with(&[[0xFF; 32]], &key), Err(KeyError::Forged));
    }

    #[test]
    fn what_is_not_a_key_says_so() {
        for text in ["", "hello", "imago-abc", "sk-or-v1-0123", "IMAGO"] {
            assert_eq!(
                read_with(&trusted(), text),
                Err(KeyError::NotAKey),
                "{text:?}"
            );
        }
        let key = issue(&SEED, 1, 2, "Cut").expect("issued");
        for text in [
            "IMAGO-",
            "IMAGO-!!!",
            "IMAGO-AAAA",
            &key[..key.len() - 30],
            // Longer than any key: 13 bytes, a 65-byte name and a signature.
            &format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode([1; 142])),
        ] {
            assert_eq!(
                read_with(&trusted(), text),
                Err(KeyError::Damaged),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_signed_key_in_a_format_from_the_future_asks_for_a_newer_app() {
        let mut bytes = vec![VERSION + 1];
        bytes.extend([0; 12]);
        let message = [CONTEXT, &bytes].concat();
        let signature = KeyPair::from_seed(Seed::new(SEED)).sk.sign(message, None);
        bytes.extend(signature.as_ref());
        let text = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
        assert_eq!(read_with(&trusted(), &text), Err(KeyError::Newer));
    }

    #[test]
    fn a_signature_for_something_else_is_not_a_key() {
        // The key's bytes signed without the context before them.
        let mut bytes = vec![VERSION];
        bytes.extend([0; 12]);
        let signature = KeyPair::from_seed(Seed::new(SEED)).sk.sign(&bytes, None);
        bytes.extend(signature.as_ref());
        let text = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
        assert_eq!(read_with(&trusted(), &text), Err(KeyError::Forged));
    }

    #[test]
    fn the_app_trusts_real_signing_keys_and_no_test_key() {
        for key in PUBLIC_KEYS {
            assert_ne!(key, [0; 32], "run `supporter-key keygen` and list its key");
            assert!(PublicKey::from_slice(&key).is_ok());
            assert_ne!(key, public_key(&SEED));
        }
        let key = issue(&SEED, 1, 2, "Test").expect("issued");
        assert_eq!(read(&key), Err(KeyError::Forged));
    }

    #[test]
    fn new_keys_and_numbers_differ() {
        assert_ne!(new_seed(), new_seed());
        assert_ne!(new_id(), new_id());
    }

    #[test]
    fn days_are_dates() {
        assert_eq!(date(0), (1970, 1, 1));
        assert_eq!(date(59), (1970, 3, 1));
        assert_eq!(date(11_016), (2000, 2, 29));
        assert_eq!(date(20_768), (2026, 11, 11));
        // The last day there is a number for is still a date.
        let (year, month, day) = date(u32::MAX);
        assert!(year > 11_000_000 && (1..=12).contains(&month) && (1..=31).contains(&day));
        // A day after another is the next date.
        assert_eq!(date(20_869), (2027, 2, 20));
        assert_eq!(date(20_878), (2027, 3, 1));
    }
}
