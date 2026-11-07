//! Standard base64 with padding, for screenshots crossing WebDriver and the worker frames.
//! Written out here so the worker needs no encoding dependency.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes `data` with padding.
#[must_use]
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let bytes = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let group = u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2]);
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(char::from(
                    ALPHABET[(group >> (18 - 6 * index) & 63) as usize],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decodes padded base64; `None` for any other character or a malformed length.
#[must_use]
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (number, chunk) in bytes.chunks(4).enumerate() {
        let last = number + 1 == bytes.len() / 4;
        let padding = chunk.iter().rev().take_while(|byte| **byte == b'=').count();
        if padding > 2 || (padding > 0 && !last) {
            return None;
        }
        let mut group = 0u32;
        for &byte in &chunk[..4 - padding] {
            let value = ALPHABET.iter().position(|letter| *letter == byte)?;
            group = group << 6 | value as u32;
        }
        group <<= 6 * padding as u32;
        let decoded = group.to_be_bytes();
        out.extend_from_slice(&decoded[1..4 - padding]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_padding() {
        for (plain, coded) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foobar", "Zm9vYmFy"),
            (b"\xff\x00\xfe", "/wD+"),
        ] {
            assert_eq!(encode(plain), coded);
            assert_eq!(decode(coded).as_deref(), Some(plain));
        }
        assert!(decode("Zm9").is_none());
        assert!(decode("Zg==Zg==").is_none());
        assert!(decode("Z$==").is_none());
    }
}
