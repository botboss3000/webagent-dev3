//! RFC 6238 one-time codes (SHA-1, 30 s, 6 digits) from a base32 authenticator secret.
//! SHA-1 and HMAC are written out here because the worker has no SHA-1 dependency; the
//! secret and the codes never leave the browser session.

/// SHA-1 of `data` (FIPS 180-4).
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_be_bytes());
    for block in message.chunks_exact(64) {
        let mut words = [0u32; 80];
        for (index, word) in block.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (index, word) in words.iter().enumerate() {
            let (f, k) = match index {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, next);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut digest = [0u8; 20];
    for (chunk, value) in digest.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    digest
}

/// HMAC-SHA1 (RFC 2104).
fn hmac_sha1(key: &[u8], message: &[u8]) -> [u8; 20] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..20].copy_from_slice(&sha1(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner: Vec<u8> = block.iter().map(|byte| byte ^ 0x36).collect();
    inner.extend_from_slice(message);
    let mut outer: Vec<u8> = block.iter().map(|byte| byte ^ 0x5c).collect();
    outer.extend_from_slice(&sha1(&inner));
    sha1(&outer)
}

/// Python's `base64.b32decode` of an upper-case ASCII string, padding rules included.
fn base32(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let stripped = text.trim_end_matches('=').as_bytes();
    let padding = bytes.len() - stripped.len();
    let mut decoded = Vec::with_capacity(stripped.len() * 5 / 8 + 5);
    let mut last = 0u64;
    for quantum in stripped.chunks(8) {
        let mut acc = 0u64;
        for &byte in quantum {
            let value = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'2'..=b'7' => byte - b'2' + 26,
                _ => return None,
            };
            acc = (acc << 5) | u64::from(value);
        }
        last = acc;
        decoded.extend_from_slice(&acc.to_be_bytes()[3..]);
    }
    if !bytes.len().is_multiple_of(8) || ![0, 1, 3, 4, 6].contains(&padding) {
        return None;
    }
    if padding > 0 && !decoded.is_empty() {
        let shifted = (last << (5 * padding)).to_be_bytes();
        let keep = (43 - 5 * padding) / 8;
        let start = decoded.len() - 5;
        decoded.truncate(start);
        decoded.extend_from_slice(&shifted[3..3 + keep]);
    }
    Some(decoded)
}

/// The current code for `secret` at Unix time `at`; `None` when the secret is not base32
/// (the caller rejects it as "not an authenticator secret").
#[must_use]
pub fn code(secret: &str, at: f64) -> Option<String> {
    let text: String = secret
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(char::to_uppercase)
        .collect();
    if !text.is_ascii() {
        return None;
    }
    let padded = format!("{text}{}", "=".repeat((8 - text.len() % 8) % 8));
    let key = base32(&padded)?;
    let counter = (at / 30.0).floor() as u64;
    let digest = hmac_sha1(&key, &counter.to_be_bytes());
    let offset = usize::from(digest[19] & 15);
    let value = u32::from_be_bytes([
        digest[offset],
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]) & 0x7fff_ffff;
    Some(format!("{:06}", value % 1_000_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_and_hmac_match_known_vectors() {
        assert_eq!(
            hex::encode(sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex::encode(sha1(&[b'a'; 1000])),
            "291e9a6c66994949b57ba5e650361e98fc36b1ba"
        );
        assert_eq!(
            hex::encode(hmac_sha1(
                b"key",
                b"The quick brown fox jumps over the lazy dog"
            )),
            "de7c9b85b8b78aa6bc8a7a36f70a90701c9db4d9"
        );
    }

    #[test]
    fn codes_match_the_rfc_vector() {
        assert_eq!(
            code("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", 59.0).as_deref(),
            Some("287082")
        );
        assert_eq!(
            code("gezd gnbv-gy3t qojq gezd gnbv gy3t qojq", 59.0).as_deref(),
            Some("287082")
        );
        assert_eq!(
            code("12345678901234567890", 1_111_111_109.0),
            None,
            "digits 0, 1, 8 and 9 are not base32"
        );
        assert!(code("not base32!", 59.0).is_none());
        // Python accepts these lengths' padding and rejects the others.
        assert!(code("MZXW6", 0.0).is_some());
        assert!(code("MZXW6Y", 0.0).is_none());
        assert_eq!(base32("MZXW6YTB"), Some(b"fooba".to_vec()));
        assert_eq!(base32("MZXW6==="), Some(b"foo".to_vec()));
        assert_eq!(base32("MY======"), Some(b"f".to_vec()));
    }
}
