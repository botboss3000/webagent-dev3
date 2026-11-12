//! BERT WordPiece tokenization for bge-small-en-v1.5, matching its `tokenizer.json` (uncased):
//! drop control characters, space out CJK characters, lowercase and strip accents, split on
//! whitespace and punctuation, then take the longest vocabulary pieces from the left.
use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::{anyhow, Result};
use regex::Regex;

/// Token ids the model accepts at most, including `[CLS]` and `[SEP]`.
const MAX_TOKENS: usize = 512;
/// Longer words become `[UNK]` whole.
const MAX_WORD_CHARS: usize = 100;

/// Unicode classes the BERT rules need; `std` only knows their ASCII parts.
static PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{P}$").expect("valid pattern"));
static CONTROL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\p{C}$").expect("valid pattern"));
static MARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\p{Mn}$").expect("valid pattern"));

/// A WordPiece vocabulary.
pub struct Tokenizer {
    vocab: HashMap<String, i64>,
    unknown: i64,
    start: i64,
    end: i64,
}

impl Tokenizer {
    /// The vocabulary from `vocab.txt`: one piece per line, its id the line number.
    pub fn from_vocab(text: &str) -> Result<Self> {
        let vocab: HashMap<String, i64> = text
            .lines()
            .zip(0..)
            .map(|(piece, id)| (piece.to_owned(), id))
            .collect();
        let id = |piece: &str| {
            vocab
                .get(piece)
                .copied()
                .ok_or_else(|| anyhow!("Vocabulary lacks {piece}"))
        };
        Ok(Self {
            unknown: id("[UNK]")?,
            start: id("[CLS]")?,
            end: id("[SEP]")?,
            vocab,
        })
    }

    /// `[CLS]`, the text's pieces (truncated to fit) and `[SEP]`.
    pub fn encode(&self, text: &str) -> Vec<i64> {
        let mut ids = vec![self.start];
        for word in words(&normalize(text)) {
            self.pieces(&word, &mut ids);
        }
        ids.truncate(MAX_TOKENS - 1);
        ids.push(self.end);
        ids
    }

    /// Greedy longest-match WordPiece; a word with an unknown remainder is `[UNK]` whole.
    fn pieces(&self, word: &str, ids: &mut Vec<i64>) {
        let chars: Vec<char> = word.chars().collect();
        if chars.len() > MAX_WORD_CHARS {
            ids.push(self.unknown);
            return;
        }
        let mut found = Vec::new();
        let mut start = 0;
        while start < chars.len() {
            let piece = (start + 1..=chars.len()).rev().find_map(|end| {
                let text: String = chars[start..end].iter().collect();
                let key = if start > 0 { format!("##{text}") } else { text };
                self.vocab.get(&key).map(|id| (*id, end))
            });
            let Some((id, end)) = piece else {
                ids.push(self.unknown);
                return;
            };
            found.push(id);
            start = end;
        }
        ids.extend(found);
    }
}

/// BertNormalizer: clean text, space out CJK characters, lowercase, strip accents.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\0' || c == '\u{fffd}' || (!matches!(c, '\t' | '\n' | '\r') && class(&CONTROL, c))
        {
            continue;
        }
        if c.is_whitespace() {
            out.push(' ');
        } else if chinese(c) {
            out.extend([' ', c, ' ']);
        } else {
            for lower in c.to_lowercase() {
                let base = unaccented(lower);
                if !class(&MARK, base) {
                    out.push(base);
                }
            }
        }
    }
    out
}

/// BertPreTokenizer: words split on whitespace, each punctuation character its own word.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    for c in text.chars() {
        if c.is_whitespace() || punctuation(c) {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            if !c.is_whitespace() {
                out.push(c.to_string());
            }
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

fn class(pattern: &Regex, c: char) -> bool {
    let mut buffer = [0; 4];
    pattern.is_match(c.encode_utf8(&mut buffer))
}

/// BERT counts every non-alphanumeric printable ASCII character as punctuation.
fn punctuation(c: char) -> bool {
    c.is_ascii_punctuation() || class(&PUNCTUATION, c)
}

fn chinese(c: char) -> bool {
    matches!(u32::from(c),
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x20000..=0x2A6DF | 0x2A700..=0x2B73F
        | 0x2B740..=0x2B81F | 0x2B820..=0x2CEAF | 0xF900..=0xFAFF | 0x2F800..=0x2FA1F)
}

/// The base letter of a precomposed lowercase Latin letter whose canonical decomposition is
/// that letter plus accents (NFD then dropping marks, without the normalization tables).
fn unaccented(c: char) -> char {
    const ACCENTED: &str =
        "àáâãäåāăąçćĉċčďèéêëēĕėęěĝğġģĥìíîïĩīĭįĵķĺļľñńņňòóôõöōŏőŕŗřśŝşšţťùúûüũūŭůűųŵýÿŷźżž";
    const BASE: &str =
        "aaaaaaaaacccccdeeeeeeeeegggghiiiiiiiijklllnnnnoooooooorrrssssttuuuuuuuuuuwyyyzzz";
    ACCENTED
        .chars()
        .zip(BASE.chars())
        .find_map(|(accented, base)| (accented == c).then_some(base))
        .unwrap_or(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokenizer() -> Tokenizer {
        let vocab = "[PAD]\n[UNK]\n[CLS]\n[SEP]\nthe\ncafe\nchat\n##ter\n##s\n,\n!\nun\n##able";
        Tokenizer::from_vocab(vocab).expect("test vocabulary")
    }

    #[test]
    fn pieces_follow_bert_rules() {
        let t = tokenizer();
        assert_eq!(
            t.encode("The CAFÉ, chatters!"),
            [2, 4, 5, 9, 6, 7, 8, 10, 3]
        );
        assert_eq!(t.encode("unable\tchatx"), [2, 11, 12, 1, 3]);
        assert_eq!(t.encode(""), [2, 3]);
    }

    #[test]
    fn accent_table_pairs_up() {
        assert_eq!(
            "àáâãäåāăąçćĉċčďèéêëēĕėęěĝğġģĥìíîïĩīĭįĵķĺļľñńņňòóôõöōŏőŕŗřśŝşšţťùúûüũūŭůűųŵýÿŷźżž"
                .chars()
                .count(),
            "aaaaaaaaacccccdeeeeeeeeegggghiiiiiiiijklllnnnnoooooooorrrssssttuuuuuuuuuuwyyyzzz"
                .chars()
                .count()
        );
        assert_eq!(normalize("Ñandú ß 中文"), "nandu ß  中  文 ");
    }

    #[test]
    fn long_text_is_truncated() {
        let ids = tokenizer().encode(&"the ".repeat(600));
        assert_eq!(ids.len(), MAX_TOKENS);
        assert_eq!(ids.last(), Some(&3));
    }
}
