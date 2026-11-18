//! Custom icons: an ability's `<id>.svg` or a group's `_group.svg`, drawn inline on the
//! Abilities page for every user. Only plain drawing markup passes; the page cleans it again.

/// The most bytes an icon file may hold.
pub const MAX_ICON_BYTES: usize = 16 * 1024;

/// Why `text` cannot be an inline icon, if it cannot: one `<svg>` element with no scripts,
/// embedded content, external links or event handlers.
pub fn svg_problem(text: &str) -> Option<&'static str> {
    if text.len() > MAX_ICON_BYTES {
        return Some("an icon is limited to 16 KB");
    }
    let lower = text.to_ascii_lowercase();
    if !lower.trim_start().starts_with("<svg") {
        return Some("an icon must be one <svg> element");
    }
    for refused in [
        "<script",
        "<foreignobject",
        "<iframe",
        "<object",
        "<embed",
        "<style",
        "javascript:",
        "<!entity",
        "<!doctype",
        "href=\"http",
        "href='http",
        "url(http",
        "data:",
    ] {
        if lower.contains(refused) {
            return Some("icons may not hold scripts, styles, embedded content or external links");
        }
    }
    let event = lower
        .split(|c: char| c.is_whitespace() || c == '<' || c == '/')
        .any(|word| word.starts_with("on") && word.contains('='));
    event.then_some("icons may not hold event handlers")
}

/// The icon at `path` when it exists and passes [`svg_problem`]; a bad file is ignored.
pub fn read(path: &std::path::Path) -> Option<String> {
    let bytes = crate::catalog::read_bytes(path, MAX_ICON_BYTES as u64).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    svg_problem(&text).is_none().then_some(text)
}

#[cfg(test)]
mod tests {
    use super::svg_problem;

    #[test]
    fn only_plain_drawing_markup_passes() {
        assert_eq!(
            svg_problem("<svg viewBox=\"0 0 24 24\"><path d=\"M0 0h24\"/></svg>"),
            None
        );
        for bad in [
            "<div/>",
            "<svg><script>x</script></svg>",
            "<svg onload=\"x()\"></svg>",
            "<svg><a href=\"javascript:x\"/></svg>",
            "<svg><style>*{}</style></svg>",
            "<svg><image href=\"data:image/png;base64,AA\"/></svg>",
            "<svg><use href=\"http://x/y\"/></svg>",
        ] {
            assert!(svg_problem(bad).is_some(), "{bad}");
        }
        assert!(svg_problem(&format!("<svg>{}</svg>", "x".repeat(16 * 1024))).is_some());
    }
}
