//! The credential guard for Git output and outgoing history, ported from the Git Control
//! adapter's `secret()`. Each pattern is matched by hand with the Python regular
//! expressions' exact semantics (ASCII case folding, leftmost non-overlapping matches);
//! every pattern is deterministic, so no backtracking is needed.

/// Public dummy credentials used by regression fixtures; a match whose value is one of
/// these is not a finding. Not a path exemption.
const DUMMY_CREDENTIALS: [&[u8]; 18] = [
    b"SECRET_CANARY",
    b"abcdefghijklmnop12345",
    b"another-secret",
    b"discovery-key",
    b"first-secret",
    b"fixture-audio-key",
    b"fixture-no-network-key",
    b"fixture-only",
    b"fixture-password",
    b"fixture-token",
    b"isolated-key",
    b"must-not-project",
    b"never-forward",
    b"never-leak-key",
    b"second-secret",
    b"test-key-no-network",
    b"test-password-only",
    b"valid-fixture-secret",
];

/// Reserved documentation hosts with deliberately trivial URL credentials (lower case).
const DOCUMENTED_URLS: [&[u8]; 3] = [
    b"https://u:p@example.test",
    b"https://user:password@example.invalid",
    // Deliberately trivial credentials in the Debian URL rejection fixture.
    b"https://user:password@deb.debian.org",
];

const PUBLIC_FIXTURE: &[u8] = b"sk-1234567890abcdef";

fn word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

/// Python's `\s` for bytes.
fn space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn starts_with_ci(data: &[u8], at: usize, prefix: &[u8]) -> bool {
    data.get(at..at + prefix.len())
        .is_some_and(|slice| slice.eq_ignore_ascii_case(prefix))
}

fn run(data: &[u8], at: usize, accept: impl Fn(u8) -> bool) -> usize {
    data[at.min(data.len())..]
        .iter()
        .take_while(|byte| accept(**byte))
        .count()
}

/// Whether `data` contains something credential-shaped.
#[must_use]
pub fn secret(data: &[u8]) -> bool {
    // The public redaction fixture, when not part of a longer token, is not a key.
    let mut cleaned = Vec::with_capacity(data.len());
    let mut at = 0;
    while at < data.len() {
        if data[at..].starts_with(PUBLIC_FIXTURE)
            && data
                .get(at + PUBLIC_FIXTURE.len())
                .is_none_or(|next| !word(*next))
        {
            cleaned.extend_from_slice(b"<public-redaction-fixture>");
            at += PUBLIC_FIXTURE.len();
        } else {
            cleaned.push(data[at]);
            at += 1;
        }
    }
    let data = &cleaned[..];
    (0..data.len()).any(|at| key_at(data, at)) || url_credentials(data) || assignments(data)
}

/// Private keys and well-known token prefixes, case-insensitively.
fn key_at(data: &[u8], at: usize) -> bool {
    if starts_with_ci(data, at, b"-----BEGIN ") {
        let mut after = at + 11;
        for kind in [&b"RSA "[..], b"EC ", b"OPENSSH "] {
            if starts_with_ci(data, after, kind) {
                if starts_with_ci(data, after + kind.len(), b"PRIVATE KEY-----") {
                    return true;
                }
                after = usize::MAX;
                break;
            }
        }
        if after != usize::MAX && starts_with_ci(data, after, b"PRIVATE KEY-----") {
            return true;
        }
    }
    if starts_with_ci(data, at, b"sk-") && run(data, at + 3, word) >= 16 {
        return true;
    }
    if starts_with_ci(data, at, b"gh")
        && data
            .get(at + 2)
            .is_some_and(|kind| b"pousr".contains(&kind.to_ascii_lowercase()))
        && data.get(at + 3) == Some(&b'_')
        && run(data, at + 4, |b| b.is_ascii_alphanumeric()) >= 20
    {
        return true;
    }
    starts_with_ci(data, at, b"AKIA") && run(data, at + 4, |b| b.is_ascii_alphanumeric()) >= 16
}

/// `(?:https?|ssh)://USER:PASS@HOST` credentials, except documented trivial ones.
fn url_credentials(data: &[u8]) -> bool {
    let mut at = 0;
    while at < data.len() {
        let scheme = [&b"https://"[..], b"http://", b"ssh://"]
            .into_iter()
            .find(|scheme| starts_with_ci(data, at, scheme));
        let Some(scheme) = scheme else {
            at += 1;
            continue;
        };
        let user = at + scheme.len();
        let user_len = run(data, user, |b| !space(b) && !b"/@:\"'".contains(&b));
        let colon = user + user_len;
        let password = colon + 1;
        let password_len = run(data, password, |b| !space(b) && !b"/@\"'".contains(&b));
        let sign = password + password_len;
        let host = sign + 1;
        let host_len = run(data, host, |b| !space(b) && !b"/\"'".contains(&b));
        if user_len > 0
            && data.get(colon) == Some(&b':')
            && password_len > 0
            && data.get(sign) == Some(&b'@')
            && host_len > 0
        {
            let found = data[at..host + host_len].to_ascii_lowercase();
            if !DOCUMENTED_URLS.contains(&&found[..]) {
                return true;
            }
            at = host + host_len;
        } else {
            at += 1;
        }
    }
    false
}

fn value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_./+!=-".contains(&byte)
}

/// `api_key = "…"`, `password: …` and similar assignments with a value of 12 or more
/// characters, quoted or ending its line, unless the value is a known fixture.
fn assignments(data: &[u8]) -> bool {
    let mut at = 0;
    while at < data.len() {
        match assignment_at(data, at) {
            Some((value, end)) => {
                if !DUMMY_CREDENTIALS.contains(&value) {
                    return true;
                }
                at = end;
            }
            None => at += 1,
        }
    }
    false
}

/// The value and end of an assignment match starting exactly at `at`.
fn assignment_at(data: &[u8], at: usize) -> Option<(&[u8], usize)> {
    if at > 0 && word(data[at - 1]) {
        return None;
    }
    let mut position = if starts_with_ci(data, at, b"api") {
        let mut next = at + 3;
        if matches!(data.get(next), Some(b'_' | b'-')) {
            next += 1;
        }
        starts_with_ci(data, next, b"key").then_some(next + 3)?
    } else if starts_with_ci(data, at, b"access") {
        let mut next = at + 6;
        if matches!(data.get(next), Some(b'_' | b'-')) {
            next += 1;
        }
        starts_with_ci(data, next, b"token").then_some(next + 5)?
    } else if starts_with_ci(data, at, b"password") {
        at + 8
    } else if starts_with_ci(data, at, b"secret") {
        at + 6
    } else {
        return None;
    };
    if matches!(data.get(position), Some(b'"' | b'\'')) {
        position += 1;
    }
    position += run(data, position, space);
    if !matches!(data.get(position), Some(b':' | b'=')) {
        return None;
    }
    position += 1;
    position += run(data, position, space);
    if matches!(data.get(position), Some(b'"' | b'\'')) {
        let start = position + 1;
        let length = run(data, start, value_byte);
        let end = start + length;
        return (length >= 12 && matches!(data.get(end), Some(b'"' | b'\'')))
            .then(|| (&data[start..end], end + 1));
    }
    let length = run(data, position, value_byte);
    let end = position + length;
    if length < 12 {
        return None;
    }
    // Lookahead: spaces or tabs, then an optional CR before the line end, or a quote.
    let after = end + run(data, end, |b| b == b' ' || b == b'\t');
    let line_end = |index: usize| index == data.len() || data[index] == b'\n';
    let ends = line_end(after)
        || data.get(after) == Some(&b'\r') && line_end(after + 1)
        || matches!(data.get(after), Some(b'"' | b'\''));
    ends.then(|| (&data[position..end], end))
}

#[cfg(test)]
mod tests {
    use super::secret;

    #[test]
    fn known_shapes_are_found() {
        let assembled = |parts: &[&str]| parts.concat().into_bytes();
        for text in [
            assembled(&["-----BEGIN ", "OPENSSH PRIVATE KEY-----"]),
            assembled(&["-----begin ", "private key-----"]),
            assembled(&["sk-", "abcdefghijklmnopq"]),
            assembled(&["ghp", "_", "abcdefghijklmnopqrstu"]),
            assembled(&["AKIA", "ABCDEFGHIJKLMNOP"]),
            assembled(&["https://", "bob:hunter2", "@git.example.com/x"]),
            assembled(&["api_key", " = \"", "abcdefghijkl0", "\""]),
            assembled(&["PASSWORD", ": ", "abcdefghijklmnop", "\r\n"]),
        ] {
            assert!(secret(&text), "{}", String::from_utf8_lossy(&text));
        }
    }

    #[test]
    fn fixtures_and_ordinary_text_pass() {
        let assembled = |parts: &[&str]| parts.concat().into_bytes();
        for text in [
            assembled(&["sk-", "1234567890abcdef"]),
            assembled(&["https://", "u:p", "@example.test"]),
            assembled(&["password", " = \"", "fixture-password", "\""]),
            assembled(&["my_password", " = ", "abcdefghijklmnop"]),
            assembled(&["password", " = ", "short"]),
            assembled(&["password", " = ", "abcdefghijklmnop", " trailing"]),
            b"let secret_value = compute();".to_vec(),
        ] {
            assert!(!secret(&text), "{}", String::from_utf8_lossy(&text));
        }
    }
}
