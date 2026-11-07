//! The small part of Python's `urllib.parse` the browser rules were written against:
//! `urlsplit`, `urlunsplit`, `parse_qsl` and `urlencode`. The rules compare split parts
//! and rebuilt URLs, so these follow Python's behaviour rather than the WHATWG parser.

/// Python raises `ValueError` here; callers report it as a backend failure of that type.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Invalid;

/// One URL split into its five parts, as `urllib.parse.urlsplit` returns them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Split {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

/// Schemes whose rebuilt URL carries `//` even with an empty network location.
const USES_NETLOC: [&str; 26] = [
    "ftp",
    "http",
    "gopher",
    "nntp",
    "telnet",
    "imap",
    "wais",
    "file",
    "mms",
    "https",
    "shttp",
    "snews",
    "prospero",
    "rtsp",
    "rtsps",
    "rtspu",
    "rsync",
    "svn",
    "svn+ssh",
    "sftp",
    "nfs",
    "git",
    "git+ssh",
    "ws",
    "wss",
    "itms-services",
];

/// `urlsplit(url)`: leading C0 controls and spaces are dropped and tabs and newlines
/// removed, as Python does; an unbalanced or invalid bracketed host is [`Invalid`].
pub(crate) fn urlsplit(url: &str) -> Result<Split, Invalid> {
    let url: String = url
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut rest = url.as_str();
    let mut split = Split::default();
    if let Some(colon) = rest.find(':') {
        let candidate = &rest[..colon];
        if colon > 0
            && candidate.starts_with(|c: char| c.is_ascii_alphabetic())
            && candidate
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            split.scheme = candidate.to_ascii_lowercase();
            rest = &rest[colon + 1..];
        }
    }
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        split.netloc = after[..end].to_owned();
        rest = &after[end..];
        let (open, close) = (split.netloc.contains('['), split.netloc.contains(']'));
        if open != close {
            return Err(Invalid);
        }
        if open {
            check_bracketed(&split.netloc)?;
        }
    }
    if let Some((before, fragment)) = rest.split_once('#') {
        split.fragment = fragment.to_owned();
        rest = before;
    }
    if let Some((before, query)) = rest.split_once('?') {
        split.query = query.to_owned();
        rest = before;
    }
    split.path = rest.to_owned();
    Ok(split)
}

/// Python's `_check_bracketed_netloc`: text before `[` or after `]` other than a port, or
/// a bracketed host that is not an IPv6 address (or `v`-prefixed future form), is invalid.
fn check_bracketed(netloc: &str) -> Result<(), Invalid> {
    let hostinfo = netloc.rsplit_once('@').map_or(netloc, |(_, host)| host);
    let Some((before, bracketed)) = hostinfo.split_once('[') else {
        return Ok(());
    };
    if !before.is_empty() {
        return Err(Invalid);
    }
    let (host, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
    if !port.is_empty() && !port.starts_with(':') {
        return Err(Invalid);
    }
    if let Some(future) = host.strip_prefix(['v', 'V']) {
        let valid = future.split_once('.').is_some_and(|(version, address)| {
            !version.is_empty()
                && version.chars().all(|c| c.is_ascii_hexdigit())
                && !address.is_empty()
                && address
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._~!$&'()*+,;=:".contains(c))
        });
        return if valid { Ok(()) } else { Err(Invalid) };
    }
    let address = host.split_once('%').map_or(host, |(address, _)| address);
    address
        .parse::<std::net::Ipv6Addr>()
        .map(|_| ())
        .map_err(|_| Invalid)
}

impl Split {
    fn hostinfo(&self) -> (&str, Option<&str>) {
        let hostinfo = self
            .netloc
            .rsplit_once('@')
            .map_or(self.netloc.as_str(), |(_, host)| host);
        let (host, port) = match hostinfo.split_once('[') {
            Some((_, bracketed)) => {
                let (host, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (host, after.split_once(':').map_or("", |(_, port)| port))
            }
            None => hostinfo.split_once(':').unwrap_or((hostinfo, "")),
        };
        (host, (!port.is_empty()).then_some(port))
    }

    /// `.hostname`: lower-cased (its `%zone` kept as written), `None` when empty.
    pub(crate) fn hostname(&self) -> Option<String> {
        let (host, _) = self.hostinfo();
        if host.is_empty() {
            return None;
        }
        Some(match host.split_once('%') {
            Some((address, zone)) => format!("{}%{zone}", address.to_lowercase()),
            None => host.to_lowercase(),
        })
    }

    /// `.port`: `None` when absent; a non-digit or out-of-range port is [`Invalid`].
    pub(crate) fn port(&self) -> Result<Option<u16>, Invalid> {
        match self.hostinfo().1 {
            None => Ok(None),
            Some(port) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
                let digits = port.trim_start_matches('0');
                if digits.len() > 5 {
                    return Err(Invalid);
                }
                digits
                    .parse::<u32>()
                    .ok()
                    .or(Some(0).filter(|_| digits.is_empty()))
                    .and_then(|port| u16::try_from(port).ok())
                    .map(Some)
                    .ok_or(Invalid)
            }
            Some(_) => Err(Invalid),
        }
    }

    /// Whether `.username` or `.password` is non-empty.
    pub(crate) fn has_userinfo(&self) -> bool {
        self.netloc
            .rsplit_once('@')
            .is_some_and(|(userinfo, _)| match userinfo.split_once(':') {
                Some((user, password)) => !user.is_empty() || !password.is_empty(),
                None => !userinfo.is_empty(),
            })
    }
}

/// `urlunsplit((scheme, netloc, path, query, fragment))`.
pub(crate) fn urlunsplit(
    scheme: &str,
    netloc: &str,
    path: &str,
    query: &str,
    fragment: &str,
) -> String {
    let mut url = path.to_owned();
    if !netloc.is_empty()
        || (!scheme.is_empty() && USES_NETLOC.contains(&scheme) && !url.starts_with("//"))
    {
        if !url.is_empty() && !url.starts_with('/') {
            url.insert(0, '/');
        }
        url = format!("//{netloc}{url}");
    }
    if !scheme.is_empty() {
        url = format!("{scheme}:{url}");
    }
    if !query.is_empty() {
        url = format!("{url}?{query}");
    }
    if !fragment.is_empty() {
        url = format!("{url}#{fragment}");
    }
    url
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// `unquote(text)` with UTF-8 and replacement characters, as Python decodes `%XX`.
pub(crate) fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' && at + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[at + 1]), hex(bytes[at + 2])) {
                out.push(high << 4 | low);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `parse_qsl(query, keep_blank_values=True)`: `&`-separated, `+` as space, empty pieces
/// skipped, names without `=` given an empty value.
pub(crate) fn parse_qsl(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|piece| !piece.is_empty())
        .map(|piece| {
            let (name, value) = piece.split_once('=').unwrap_or((piece, ""));
            (
                unquote(&name.replace('+', " ")),
                unquote(&value.replace('+', " ")),
            )
        })
        .collect()
}

/// `quote(text, safe="")`, or `quote_plus` when `plus` (spaces become `+`).
pub(crate) fn quote(text: &str, plus: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' if plus => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `urlencode(pairs)`: `quote_plus(name)=quote_plus(value)` joined by `&`.
pub(crate) fn urlencode(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(name, value)| format!("{}={}", quote(name, true), quote(value, true)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_python() {
        let split = urlsplit(concat!(
            "HTTPS://",
            "User:Pw",
            "@Example.COM:8443/a/b?x=1&y#frag"
        ))
        .expect("valid");
        assert_eq!(split.scheme, "https");
        assert_eq!(split.netloc, "User:Pw@Example.COM:8443");
        assert_eq!(split.path, "/a/b");
        assert_eq!(split.query, "x=1&y");
        assert_eq!(split.fragment, "frag");
        assert_eq!(split.hostname().as_deref(), Some("example.com"));
        assert_eq!(split.port(), Ok(Some(8443)));
        assert!(split.has_userinfo());
        assert!(!urlsplit("https://@example.com/")
            .expect("valid")
            .has_userinfo());
        assert_eq!(
            urlsplit("https://h:x/").expect("valid").port(),
            Err(Invalid)
        );
        assert_eq!(
            urlsplit("https://h:99999/").expect("valid").port(),
            Err(Invalid)
        );
        assert_eq!(urlsplit("https://h:/").expect("valid").port(), Ok(None));
        assert_eq!(urlsplit("https://[::1/"), Err(Invalid));
        assert_eq!(
            urlsplit("http://[::1]:80/")
                .expect("valid")
                .hostname()
                .as_deref(),
            Some("::1")
        );
        let relative = urlsplit("/app?tab=chat").expect("valid");
        assert_eq!(
            (relative.scheme.as_str(), relative.path.as_str()),
            ("", "/app")
        );
        assert_eq!(
            urlsplit("//outside.invalid/").expect("valid").netloc,
            "outside.invalid"
        );
        assert_eq!(urlsplit("1http://x").expect("valid").scheme, "");
    }

    #[test]
    fn rebuilds_and_encodes_like_python() {
        assert_eq!(urlunsplit("https", "h", "/p", "q=1", ""), "https://h/p?q=1");
        assert_eq!(urlunsplit("", "", "/app", "", ""), "/app");
        assert_eq!(urlunsplit("http", "", "", "", ""), "http://");
        assert_eq!(urlunsplit("about", "", "blank", "", ""), "about:blank");
        assert_eq!(
            parse_qsl("a=1&&b&c=x+y%2Fz&d=%zz"),
            vec![
                ("a".into(), "1".into()),
                ("b".into(), String::new()),
                ("c".into(), "x y/z".into()),
                ("d".into(), "%zz".into())
            ]
        );
        assert_eq!(
            urlencode(&[("path".into(), "/notes/to day.md".into())]),
            "path=%2Fnotes%2Fto+day.md"
        );
        assert_eq!(quote("a+b c~é", false), "a%2Bb%20c~%C3%A9");
    }
}
