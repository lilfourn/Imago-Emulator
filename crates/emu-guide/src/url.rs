//! Just enough of URL encoding for OpenRouter's sign-in: query values out,
//! and the browser's query back.

/// `text` percent-encoded as a URL query value: unreserved characters as
/// they are, every other byte as `%XX`.
pub fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The value of `name` in `query` (`a=1&b=2`), decoded: `+` as a space and
/// `%XX` as its byte. The first one, if `name` comes more than once.
pub fn query_value(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (decode(key) == name).then(|| decode(value))
    })
}

/// `text` with `+` as a space and each `%XX` as its byte; a `%` not
/// followed by two hex digits stays as it is.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        let hex = bytes
            .get(at + 1..at + 3)
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        match (byte, hex) {
            (b'%', Some(value)) => {
                decoded.push(value);
                at += 3;
            }
            (b'+', _) => {
                decoded.push(b' ');
                at += 1;
            }
            _ => {
                decoded.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_encoded_byte_by_byte() {
        assert_eq!(
            encode("http://localhost:4321/callback"),
            "http%3A%2F%2Flocalhost%3A4321%2Fcallback"
        );
        assert_eq!(encode("DS Emulator · Mac"), "DS%20Emulator%20%C2%B7%20Mac");
        assert_eq!(encode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn query_values_are_found_and_decoded() {
        let query = "code=abc%2B1&state=x+y&empty=&bare";
        assert_eq!(query_value(query, "code").as_deref(), Some("abc+1"));
        assert_eq!(query_value(query, "state").as_deref(), Some("x y"));
        assert_eq!(query_value(query, "empty").as_deref(), Some(""));
        assert_eq!(query_value(query, "bare").as_deref(), Some(""));
        assert_eq!(query_value(query, "missing"), None);
        assert_eq!(query_value("", "code"), None);
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz%C3%A9"), "%zzé");
    }
}
