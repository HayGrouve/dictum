use anyhow::{Context, Result, bail};

/// SentencePiece vocabulary as exported for the ONNX model (`<token> <id>` per line).
pub(crate) struct Vocab {
    tokens: Vec<String>,
    blank: usize,
}

pub(crate) const WORD_START: char = '\u{2581}'; // SentencePiece "▁"

impl Vocab {
    pub(crate) fn parse(text: &str) -> Result<Self> {
        let mut entries = Vec::new();
        for (line_no, line) in text.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            let (token, id) =
                line.rsplit_once(' ').with_context(|| format!("vocab line {} is malformed", line_no + 1))?;
            let id: usize = id.parse().with_context(|| format!("vocab line {} has a bad id", line_no + 1))?;
            entries.push((id, token.to_string()));
        }
        let size = entries.iter().map(|(id, _)| id + 1).max().unwrap_or(0);
        if size == 0 {
            bail!("vocab is empty");
        }
        let mut tokens = vec![String::new(); size];
        for (id, token) in entries {
            tokens[id] = token;
        }
        let blank = tokens.iter().position(|t| t == "<blk>").context("vocab has no <blk> token")?;
        Ok(Self { tokens, blank })
    }

    pub(crate) fn len(&self) -> usize {
        self.tokens.len()
    }

    pub(crate) fn blank(&self) -> usize {
        self.blank
    }

    pub(crate) fn token(&self, id: usize) -> Option<&str> {
        self.tokens.get(id).map(String::as_str)
    }

    /// Turns token ids into text, the same way SentencePiece does, plus a small clean-up for
    /// punctuation that the model occasionally emits as a separate "word".
    pub(crate) fn decode(&self, ids: &[usize]) -> String {
        let mut raw = String::new();
        for &id in ids {
            let Some(token) = self.tokens.get(id) else { continue };
            if is_special(token) {
                continue;
            }
            raw.extend(token.chars().map(|c| if c == WORD_START { ' ' } else { c }));
        }
        tidy_spaces(&raw)
    }
}

fn is_special(token: &str) -> bool {
    (token.starts_with("<|") && token.ends_with("|>"))
        || matches!(token, "<unk>" | "<pad>" | "<blk>" | "<s>" | "</s>")
}

/// Collapses runs of whitespace, trims, and removes a space before closing punctuation.
pub(crate) fn tidy_spaces(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending_space = false;
    for c in raw.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space && !matches!(c, ',' | '.' | '!' | '?' | ';' | ':' | ')' | ']' | '}' | '%') {
            out.push(' ');
        }
        pending_space = false;
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab() -> Vocab {
        Vocab::parse("<unk> 0\n<|en|> 1\n▁Hello 2\n, 3\n▁world 4\n▁. 5\n▁it 6\n's 7\n<blk> 8\n").unwrap()
    }

    #[test]
    fn parses_blank_and_size() {
        let v = vocab();
        assert_eq!(v.len(), 9);
        assert_eq!(v.blank(), 8);
    }

    #[test]
    fn decodes_like_sentencepiece() {
        let v = vocab();
        assert_eq!(v.decode(&[2, 3, 4, 5]), "Hello, world.");
        assert_eq!(v.decode(&[6, 7]), "it's");
    }

    #[test]
    fn skips_special_tokens() {
        let v = vocab();
        assert_eq!(v.decode(&[1, 0, 2, 8, 4]), "Hello world");
    }

    #[test]
    fn rejects_vocab_without_blank() {
        assert!(Vocab::parse("a 0\nb 1\n").is_err());
    }

    #[test]
    fn tidies_whitespace() {
        assert_eq!(tidy_spaces("  a  b , c  "), "a b, c");
        assert_eq!(tidy_spaces(""), "");
    }
}
