//! A file's named blocks with their line ranges, found by plain scanning: functions,
//! classes and impls in brace languages and Python, rule sets in CSS, headings in Markdown.
//! It guides reads to the right span; it is not a parser, so unusual syntax can be missed.
use std::path::Path;

/// One named block; lines are 1-based and inclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Block {
    pub start: usize,
    pub end: usize,
    pub depth: usize,
    pub name: String,
}

const NAME_CHARS: usize = 80;
const RUST_ITEMS: [&str; 7] = [
    "fn ",
    "impl",
    "struct ",
    "enum ",
    "trait ",
    "mod ",
    "macro_rules!",
];
const JS_NOT_METHODS: [&str; 8] = [
    "if", "for", "while", "switch", "catch", "return", "function", "with",
];

/// The named blocks of `text`, ordered by start line.
pub(super) fn blocks(label: &str, text: &str) -> Vec<Block> {
    let extension = Path::new(label)
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase());
    let lines: Vec<&str> = text.lines().collect();
    match extension.as_deref() {
        Some("md") => headings(&lines),
        Some("py") => python(&lines),
        Some("css") => braces(&lines, css_header),
        Some("rs") => braces(&lines, rust_header),
        Some("js" | "mjs" | "cjs" | "ts" | "jsx" | "tsx") => braces(&lines, js_header),
        _ => Vec::new(),
    }
}

/// The innermost block containing `line`.
pub(super) fn enclosing(blocks: &[Block], line: usize) -> Option<&Block> {
    blocks
        .iter()
        .filter(|block| block.start <= line && line <= block.end)
        .max_by_key(|block| block.start)
}

/// One line per block, indented by nesting: `40-88 fn render`.
pub(super) fn render(blocks: &[Block], limit: usize) -> String {
    let mut lines: Vec<String> = blocks
        .iter()
        .take(limit)
        .map(|block| {
            format!(
                "{}{}-{} {}",
                "  ".repeat(block.depth.min(6)),
                block.start,
                block.end,
                block.name
            )
        })
        .collect();
    if blocks.len() > limit {
        lines.push(format!("… {} more blocks", blocks.len() - limit));
    }
    lines.join("\n")
}

fn short(text: &str) -> String {
    let text = text.trim().trim_end_matches('{').trim();
    let mut name: String = text.chars().take(NAME_CHARS).collect();
    if text.chars().count() > NAME_CHARS {
        name.push('…');
    }
    name
}

fn headings(lines: &[&str]) -> Vec<Block> {
    let mut found: Vec<(usize, usize, String)> = Vec::new();
    let mut fenced = false;
    for (index, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let level = line.chars().take_while(|c| *c == '#').count();
        if !fenced && (1..=6).contains(&level) && line[level..].starts_with(' ') {
            found.push((index + 1, level, short(line)));
        }
    }
    found
        .iter()
        .enumerate()
        .map(|(at, (start, level, name))| {
            let end = found[at + 1..]
                .iter()
                .find(|(_, other, _)| other <= level)
                .map_or(lines.len(), |(next, _, _)| next - 1);
            Block {
                start: *start,
                end,
                depth: level - 1,
                name: name.clone(),
            }
        })
        .collect()
}

fn python(lines: &[&str]) -> Vec<Block> {
    let indent = |line: &str| line.len() - line.trim_start().len();
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if !["def ", "async def ", "class "]
            .iter()
            .any(|key| trimmed.starts_with(key))
        {
            continue;
        }
        let own = indent(line);
        let mut end = index + 1;
        for (offset, next) in lines[index + 1..].iter().enumerate() {
            if next.trim().is_empty() {
                continue;
            }
            if indent(next) <= own {
                break;
            }
            end = index + offset + 2;
        }
        found.push(Block {
            start: index + 1,
            end,
            depth: own / 4,
            name: short(trimmed),
        });
    }
    found
}

fn css_header(trimmed: &str) -> Option<String> {
    (!trimmed.starts_with('}') && trimmed.contains('{'))
        .then(|| short(&trimmed[..trimmed.find('{').unwrap_or(0)]))
}

fn rust_header(trimmed: &str) -> Option<String> {
    let mut rest = trimmed;
    for prefix in [
        "pub(crate) ",
        "pub(super) ",
        "pub ",
        "const ",
        "async ",
        "unsafe ",
        "extern \"C\" ",
    ] {
        rest = rest.strip_prefix(prefix).unwrap_or(rest);
    }
    RUST_ITEMS
        .iter()
        .any(|item| rest.starts_with(item))
        .then(|| short(trimmed))
}

fn js_header(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    let rest = rest.strip_prefix("default ").unwrap_or(rest);
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    if rest.starts_with("function") || rest.starts_with("class ") {
        return Some(short(trimmed));
    }
    if ["const ", "let ", "var "]
        .iter()
        .any(|key| rest.starts_with(key))
        && (rest.contains("=>") || rest.contains("function"))
    {
        return Some(short(
            &trimmed[..trimmed.find('=').unwrap_or(trimmed.len())],
        ));
    }
    // A method or object function: `name(args) {` or `name: function`.
    let word: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    let after = rest[word.len()..].trim_start();
    if !word.is_empty()
        && !JS_NOT_METHODS.contains(&word.as_str())
        && after.starts_with('(')
        && trimmed.ends_with('{')
    {
        return Some(short(
            &trimmed[..trimmed.find('(').unwrap_or(trimmed.len())],
        ));
    }
    None
}

/// Net brace change on one line, skipping strings and `//` comments.
fn brace_delta(line: &str) -> (i64, bool) {
    let (mut delta, mut opened, mut quote, mut escaped) = (0i64, false, None::<char>, false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == open {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '`' => quote = Some(c),
            // A Rust lifetime or char literal both start with '; only quote a closed one.
            '\'' if line.matches('\'').count() % 2 == 0 => quote = Some(c),
            '/' if chars.peek() == Some(&'/') => break,
            '{' => {
                delta += 1;
                opened = true;
            }
            '}' => delta -= 1,
            _ => {}
        }
    }
    (delta, opened)
}

fn braces(lines: &[&str], header: fn(&str) -> Option<String>) -> Vec<Block> {
    let mut found = Vec::new();
    // Open blocks: (index in `found`, depth the block closes at).
    let mut open: Vec<(usize, i64)> = Vec::new();
    let mut pending: Option<(usize, String, usize)> = None;
    let mut depth = 0i64;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some(name) = header(trimmed) {
            pending = Some((index + 1, name, 0));
        }
        let (delta, opened) = brace_delta(trimmed);
        if opened {
            if let Some((start, name, _)) = pending.take() {
                found.push(Block {
                    start,
                    end: start,
                    depth: open.len(),
                    name,
                });
                open.push((found.len() - 1, depth));
            }
        } else if let Some((_, _, waited)) = pending.as_mut() {
            // A header's brace may follow on the next lines (`where` clauses, wrapped args).
            *waited += 1;
            if *waited > 3 || trimmed.ends_with(';') {
                pending = None;
            }
        }
        depth += delta;
        while let Some(&(at, closes)) = open.last() {
            if depth > closes {
                break;
            }
            found[at].end = index + 1;
            open.pop();
        }
    }
    for (at, _) in open {
        found[at].end = lines.len();
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(label: &str, text: &str) -> Vec<(usize, usize, String)> {
        blocks(label, text)
            .into_iter()
            .map(|b| (b.start, b.end, b.name))
            .collect()
    }

    #[test]
    fn javascript_functions_and_methods() {
        let text = "'use strict';\nfunction open(x) {\n  if (x) {\n    go();\n  }\n}\nconst close = () => {\n  stop('}');\n};\nclass A {\n  run(a) {\n    return 1;\n  }\n}\n";
        assert_eq!(
            names("a.js", text),
            vec![
                (2, 6, "function open(x)".into()),
                (7, 9, "const close".into()),
                (10, 14, "class A".into()),
                (11, 13, "run".into()),
            ]
        );
    }

    #[test]
    fn css_rules_and_media_nesting() {
        let text =
            "#a { color: red; }\n@media (max-width: 9px) {\n  .b,\n  .c {\n    top: 0;\n  }\n}\n";
        let found = names("x.css", text);
        assert_eq!(found[0], (1, 1, "#a".into()));
        assert_eq!(found[1], (2, 7, "@media (max-width: 9px)".into()));
        assert_eq!(found[2], (4, 6, ".c".into()));
    }

    #[test]
    fn rust_items_with_late_brace() {
        let text = "pub fn a<'x>(v: &'x str)\nwhere\n    T: Copy,\n{\n    let c = '{';\n}\nimpl B {\n    fn c() {}\n}\n";
        let found = names("m.rs", text);
        assert_eq!(found[0].0..=found[0].1, 1..=6);
        assert_eq!(found[1], (7, 9, "impl B".into()));
        assert_eq!(found[2], (8, 8, "fn c() {}".into()));
    }

    #[test]
    fn markdown_sections_and_python_defs() {
        let md = "# T\nx\n## A\ny\n```\n# not\n```\n## B\nz\n";
        assert_eq!(
            names("r.md", md),
            vec![
                (1, 9, "# T".into()),
                (3, 7, "## A".into()),
                (8, 9, "## B".into())
            ]
        );
        let py = "class K:\n    def m(self):\n        return 1\n\nx = 2\n";
        assert_eq!(
            names("p.py", py),
            vec![(1, 3, "class K:".into()), (2, 3, "def m(self):".into())]
        );
    }

    #[test]
    fn enclosing_picks_innermost_and_render_indents() {
        let found = blocks("a.js", "class A {\n  run() {\n    x();\n  }\n}\n");
        assert_eq!(enclosing(&found, 3).map(|b| b.name.as_str()), Some("run"));
        assert_eq!(render(&found, 1), "1-5 class A\n… 1 more blocks");
    }
}
