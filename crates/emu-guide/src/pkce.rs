//! PKCE ([RFC 7636](https://www.rfc-editor.org/rfc/rfc7636)): a random
//! verifier the app keeps to itself, and its S256 challenge, which goes in
//! the authorization URL. A code the browser brings back is worth a key
//! only with the verifier, so a code caught on the way is useless.

use std::fs::File;
use std::io::{self, Read};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// `N` bytes from the system's random generator.
///
/// # Errors
///
/// Fails if `/dev/urandom` cannot be read.
pub fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// A fresh verifier: 32 random bytes in unpadded base64url, 43 characters,
/// the shortest RFC 7636 allows and as random as its longest.
///
/// # Errors
///
/// As [`random_bytes`].
pub fn verifier() -> io::Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes::<32>()?))
}

/// The S256 challenge for `verifier`: its SHA-256 in unpadded base64url.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_matches_the_rfcs_example() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifiers_are_long_enough_unreserved_and_new_each_time() {
        let first = verifier().unwrap();
        assert_eq!(first.len(), 43);
        assert!(
            first
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        );
        assert_ne!(first, verifier().unwrap());
    }
}
