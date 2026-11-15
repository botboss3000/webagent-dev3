//! Escape-first Markdown for Wiki articles: article text is HTML-escaped before any tag
//! is added, and only the tags written here are emitted, so a body cannot inject markup.
//! It covers what authors use (headings, emphasis, code, links, `[[wiki links]]`, lists,
//! quotes, rules, images); it is deliberately not a full CommonMark parser.

/// Escapes text for HTML content and quoted attribute values.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// `url` when it is relative or http(s), mailto or tel; otherwise `#`.
fn safe_url(url: &str) -> String {
    let url = url.trim();
    let scheme = url.split('/').next().unwrap_or("");
    let lower = url.to_ascii_lowercase();
    let allowed = ["http:", "https:", "mailto:", "tel:"]
        .iter()
        .any(|prefix| lower.starts_with(prefix));
    if url.is_empty() || (scheme.contains(':') && !allowed) {
        "#".to_owned()
    } else {
        escape(url)
    }
}

/// Renders an article body to HTML. `resolve` maps a `[[target]]` (a title or slug,
/// lowercased) to the slug of an article the reader may open.
pub fn render(body: &str, resolve: &dyn Fn(&str) -> Option<String>) -> String {
    let body = body.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = body.split('\n').collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if let Some(lang) = trimmed.strip_prefix("```") {
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() && lines[i].trim() != "```" {
                code.push(lines[i]);
                i += 1;
            }
            i += 1;
            let class = match lang.trim() {
                "" => String::new(),
                lang => format!(" class=\"language-{}\"", escape(lang)),
            };
            out.push(format!(
                "<pre><code{class}>{}</code></pre>",
                escape(&code.join("\n"))
            ));
            continue;
        }
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        if let Some((level, text)) = heading(line) {
            out.push(format!("<h{level}>{}</h{level}>", inline(text, resolve)));
            i += 1;
            continue;
        }
        if rule(line) {
            out.push("<hr>".to_owned());
            i += 1;
            continue;
        }
        if trimmed.starts_with('>') {
            let mut quoted = Vec::new();
            while i < lines.len() {
                let Some(rest) = lines[i].trim_start().strip_prefix('>') else {
                    break;
                };
                quoted.push(inline(rest.strip_prefix(' ').unwrap_or(rest), resolve));
                i += 1;
            }
            out.push(format!("<blockquote>{}</blockquote>", quoted.join("<br>")));
            continue;
        }
        let lists: [(&str, fn(&str) -> Option<&str>); 2] = [("ul", bullet), ("ol", numbered)];
        if let Some((tag, item)) = lists.into_iter().find(|(_, item)| item(line).is_some()) {
            let mut items = Vec::new();
            while let Some(text) = lines.get(i).and_then(|line| item(line)) {
                items.push(format!("<li>{}</li>", inline(text, resolve)));
                i += 1;
            }
            out.push(format!("<{tag}>{}</{tag}>", items.concat()));
            continue;
        }
        // The first line always joins, so every pass consumes at least one line.
        let mut paragraph = vec![inline(line, resolve)];
        i += 1;
        while i < lines.len() && !lines[i].trim().is_empty() && !block_start(lines[i]) {
            paragraph.push(inline(lines[i], resolve));
            i += 1;
        }
        out.push(format!("<p>{}</p>", paragraph.join("<br>")));
    }
    out.join("\n")
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    let rest = &line[level..];
    ((1..=6).contains(&level) && rest.starts_with([' ', '\t'])).then(|| (level, rest.trim()))
}
fn rule(line: &str) -> bool {
    let line = line.trim();
    let mut chars = line.chars().filter(|c| !c.is_whitespace());
    let Some(first) = chars.next().filter(|c| matches!(c, '-' | '*' | '_')) else {
        return false;
    };
    line.chars().filter(|&c| c == first).count() >= 3 && chars.all(|c| c == first)
}
fn bullet(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let rest = line.strip_prefix(['-', '*', '+'])?;
    rest.starts_with([' ', '\t']).then(|| rest.trim_start())
}
fn numbered(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let rest = line.get(digits..)?.strip_prefix('.')?;
    (digits > 0 && rest.starts_with([' ', '\t'])).then(|| rest.trim_start())
}
fn block_start(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```")
        || trimmed.starts_with('>')
        || heading(line).is_some()
        || rule(line)
        || bullet(line).is_some()
        || numbered(line).is_some()
}

/// The text between `open` at the start of `text` and the next `close`, and what follows.
fn delimited<'a>(text: &'a str, open: &str, close: &str) -> Option<(&'a str, &'a str)> {
    let rest = text.strip_prefix(open)?;
    let end = rest.find(close)?;
    (end > 0).then(|| (&rest[..end], &rest[end + close.len()..]))
}
/// `[label](url)` at the start of `text`: label, url and what follows.
fn link(text: &str) -> Option<(&str, &str, &str)> {
    let (label, rest) = delimited(text, "[", "](")?;
    let end = rest.find(')')?;
    let url = &rest[..end];
    (!url.is_empty() && !url.contains(char::is_whitespace) && !label.contains(']'))
        .then(|| (label, url, &rest[end + 1..]))
}

fn inline(text: &str, resolve: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    let mut previous = ' ';
    while let Some(c) = rest.chars().next() {
        if let Some((code, after)) = delimited(rest, "`", "`") {
            out.push_str(&format!("<code>{}</code>", escape(code)));
            rest = after;
        } else if let Some((alt, url, after)) = rest.strip_prefix('!').and_then(link) {
            out.push_str(&format!(
                "<img src=\"{}\" alt=\"{}\" loading=\"lazy\">",
                safe_url(url),
                escape(alt)
            ));
            rest = after;
        } else if let Some((inner, after)) = delimited(rest, "[[", "]]") {
            let (target, label) = inner.split_once('|').unwrap_or((inner, inner));
            let label = escape(label.trim());
            out.push_str(&match resolve(&target.trim().to_lowercase()) {
                Some(slug) => format!(
                    "<a class=\"wiki-link\" data-wiki-slug=\"{0}\" href=\"{1}\">{label}</a>",
                    escape(&slug),
                    escape(&crate::wiki::article_url(&slug))
                ),
                // No readable article: show the label without a dead link.
                None => format!("<span class=\"wiki-link-missing\">{label}</span>"),
            });
            rest = after;
        } else if let Some((label, url, after)) = link(rest) {
            let url = safe_url(url);
            let external = if url.starts_with("http") {
                " target=\"_blank\" rel=\"noopener noreferrer nofollow\""
            } else {
                ""
            };
            out.push_str(&format!(
                "<a href=\"{url}\"{external}>{}</a>",
                inline(label, resolve)
            ));
            rest = after;
        } else if let Some((inner, after)) =
            delimited(rest, "**", "**").or_else(|| delimited(rest, "__", "__"))
        {
            out.push_str(&format!("<strong>{}</strong>", inline(inner, resolve)));
            rest = after;
        } else if let Some((inner, after)) = delimited(rest, "*", "*").or_else(|| {
            // `_` emphasis only at word edges, so snake_case names stay intact.
            delimited(rest, "_", "_")
                .filter(|_| !previous.is_alphanumeric())
                .filter(|(_, after)| !after.starts_with(char::is_alphanumeric))
        }) {
            out.push_str(&format!("<em>{}</em>", inline(inner, resolve)));
            rest = after;
        } else {
            out.push_str(&escape(&c.to_string()));
            rest = &rest[c.len_utf8()..];
            previous = c;
            continue;
        }
        previous = ' ';
    }
    out
}

/// A plain-text summary of a body for page descriptions: markup removed, at most
/// `limit` characters, cut at a word.
#[must_use]
pub fn description(body: &str, limit: usize) -> String {
    let mut text = String::new();
    let mut in_code = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if !in_code {
            text.push(' ');
            text.push_str(line);
        }
    }
    let text: String = text
        .chars()
        .filter(|c| !matches!(c, '#' | '*' | '_' | '`' | '>' | '[' | ']'))
        .collect();
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut out = String::new();
    for word in words {
        if out.chars().count() + word.chars().count() + 1 > limit {
            out.push('…');
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn article_text_cannot_inject_markup_or_scripts() {
        let html = render(
            "<script>alert(1)</script>\n[x](javascript:alert(1)) ![i](data:text/html,1)",
            &none,
        );
        assert!(!html.contains("<script"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("<a href=\"#\">x</a>"));
        assert!(html.contains("<img src=\"#\""));
        let quoted = render("[a](/x\"onclick=\"y)", &none);
        assert!(!quoted.contains("\"onclick"));
    }

    #[test]
    fn blocks_and_inline_markup_render() {
        let html = render(
            "# Title\n\nSome **bold** and *it* and `a<b`.\n\n- one\n- two\n\n1. first\n\n> quote\n\n---\n\n```rust\nlet x = 1 < 2;\n```",
            &none,
        );
        for part in [
            "<h1>Title</h1>",
            "<strong>bold</strong>",
            "<em>it</em>",
            "<code>a&lt;b</code>",
            "<ul><li>one</li><li>two</li></ul>",
            "<ol><li>first</li></ol>",
            "<blockquote>quote</blockquote>",
            "<hr>",
            "<pre><code class=\"language-rust\">let x = 1 &lt; 2;</code></pre>",
        ] {
            assert!(html.contains(part), "{part} missing from {html}");
        }
        assert!(render("snake_case_name", &none).contains("snake_case_name"));
    }

    #[test]
    fn wiki_links_open_readable_articles_only() {
        let resolve =
            |target: &str| (target == "release pipeline").then(|| "release-pipeline".to_owned());
        let html = render("See [[Release Pipeline]] and [[Hidden|secret]].", &resolve);
        assert!(html.contains(
            "<a class=\"wiki-link\" data-wiki-slug=\"release-pipeline\" href=\"/wiki/release-pipeline\">Release Pipeline</a>"
        ));
        assert!(html.contains("<span class=\"wiki-link-missing\">secret</span>"));
        let external = render("[site](https://example.test)", &none);
        assert!(external.contains("rel=\"noopener noreferrer nofollow\""));
    }

    #[test]
    fn descriptions_drop_markup_and_cut_at_words() {
        assert_eq!(
            description("# Hi\n\n**Bold** text\n```\ncode\n```", 100),
            "Hi Bold text"
        );
        assert_eq!(description("one two three", 8), "one two…");
    }
}
