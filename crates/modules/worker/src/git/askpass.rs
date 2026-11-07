//! The private Git HTTPS credential prompt, ported from the Git Control `askpass.py`. Git
//! runs `webagent-v3` as `GIT_ASKPASS` with `V3_GIT_ASKPASS=1`; it answers only a prompt
//! for exactly the granted URL. It never logs prompts or credentials.
use super::remote::split;

/// The app executable, which answers Git's prompt when started with `V3_GIT_ASKPASS=1`.
pub(super) const HELPER: &str = "/usr/local/bin/webagent-v3";

/// Host, port and path without trailing slashes, as `urlsplit` reports them; `None` when
/// the port is invalid.
fn location(url: &super::remote::Split<'_>) -> Option<(String, u32, String)> {
    let port = match url.port {
        None | Some("") => 443,
        Some(port) => port
            .parse::<u32>()
            .ok()
            .filter(|p| port.bytes().all(|b| b.is_ascii_digit()) && *p <= 65535)?,
    };
    // `urlsplit(...).port or 443`: an explicit port 0 reads as the default.
    let port = if port == 0 { 443 } else { port };
    Some((
        url.host.to_lowercase(),
        port,
        url.path.trim_end_matches('/').to_owned(),
    ))
}

/// The answer to Git's `prompt`, or `None` when it must not be answered.
#[must_use]
pub fn answer(prompt: &str, url: &str, username: &str, token: &str) -> Option<String> {
    let rest = prompt
        .strip_suffix(": ")
        .or_else(|| prompt.strip_suffix(':'))?;
    let (kind, quoted) = rest
        .strip_prefix("Username for '")
        .map(|quoted| ("Username", quoted))
        .or_else(|| {
            rest.strip_prefix("Password for '")
                .map(|quoted| ("Password", quoted))
        })?;
    let target = quoted.strip_suffix('\'')?;
    if target.is_empty() || target.contains('\'') {
        return None;
    }
    let expected = split(url)?;
    let actual = split(target)?;
    let (user, password) = match actual.userinfo {
        Some(info) => match info.split_once(':') {
            Some((user, password)) => (user, password),
            None => (info, ""),
        },
        None => ("", ""),
    };
    if expected.scheme != "https"
        || actual.scheme != "https"
        || !password.is_empty()
        || actual.query
        || actual.fragment
    {
        return None;
    }
    if location(&actual)? != location(&expected)? {
        return None;
    }
    if !user.is_empty() && user != username {
        return None;
    }
    let value = if kind == "Username" { username } else { token };
    (!value.is_empty() && value.bytes().all(|b| (33..=126).contains(&b))).then(|| value.to_owned())
}

/// `webagent-v3` as `GIT_ASKPASS`: prints the answer and exits 0, or exits 1.
#[must_use]
pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let variable = |name: &str| std::env::var(name).unwrap_or_default();
    let value = match &args[..] {
        [prompt] => answer(
            prompt,
            &variable("V3_GIT_URL"),
            &variable("V3_GIT_USERNAME"),
            &variable("V3_GIT_TOKEN"),
        ),
        _ => None,
    };
    match value {
        Some(value) => {
            println!("{value}");
            0
        }
        None => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::answer;

    const URL: &str = "https://github.com/owner/repo";

    #[test]
    fn only_the_granted_url_is_answered() {
        let token = ["fixture", "-token"].concat();
        let ask = |prompt: &str| answer(prompt, URL, "user", &token);
        assert_eq!(
            ask("Username for 'https://github.com/owner/repo': ").as_deref(),
            Some("user")
        );
        assert_eq!(
            ask("Password for 'https://user@GitHub.com:443/owner/repo/': ").as_deref(),
            Some(token.as_str())
        );
        for refused in [
            "Password for 'https://github.com/owner/other': ",
            "Password for 'https://other@github.com/owner/repo': ",
            "Password for 'http://github.com/owner/repo': ",
            "Password for 'https://github.com:8443/owner/repo': ",
            "Password for 'https://github.com/owner/repo?x=1': ",
            "Passphrase for 'https://github.com/owner/repo': ",
            "Password for https://github.com/owner/repo: ",
        ] {
            assert_eq!(ask(refused), None, "{refused}");
        }
        assert_eq!(
            answer(
                "Password for 'https://github.com/owner/repo': ",
                URL,
                "user",
                ""
            ),
            None
        );
    }
}
