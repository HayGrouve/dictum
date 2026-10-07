//! Vocabulary boosting ("word boosting"): nudges the greedy decoder towards the user's terms.
//!
//! Each phrase is matched against the model's own SentencePiece tokens, case-insensitively and in
//! every possible segmentation. While decoding, tokens that start a phrase, or continue a phrase
//! the decoder is already part-way through, get a fixed bonus added to their logit. The bonus
//! only matters when the audio is ambiguous ("Shadn" vs "shadcn"); a confident model is not
//! overruled.

use crate::vocab::{Vocab, WORD_START};

/// Logit bonus for a token that starts or continues a phrase.
/// Tuned on synthetic dev-jargon speech and LibriSpeech: 12 fixes most misheard terms while a
/// 40-term vocabulary leaves the reference transcripts unchanged; ~15 starts turning "cloudy"
/// into "Claude".
pub(crate) const BONUS: f32 = 12.0;
/// Phrases shorter than this many letters ("tsc", "Rust") get half the bonus: short phrases fit
/// almost anywhere and are easily forced onto unrelated speech.
const SHORT_PHRASE: usize = 5;

pub(crate) struct Booster {
    /// Per phrase.
    bonus: Vec<f32>,
    /// `pieces[phrase][offset]`: tokens matching the phrase at that character offset, with
    /// their length in characters.
    pieces: Vec<Vec<Vec<(usize, usize)>>>,
    lengths: Vec<usize>,
}

/// Progress through one phrase: the next character to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Partial {
    phrase: usize,
    offset: usize,
}

impl Booster {
    /// Returns `None` when there is nothing to boost.
    pub(crate) fn new(phrases: &[String], vocab: &Vocab, bonus: f32) -> Option<Self> {
        let phrases: Vec<Vec<char>> = phrases.iter().filter_map(|p| normalize(p)).collect();
        if phrases.is_empty() || bonus <= 0.0 {
            return None;
        }
        let tokens: Vec<(usize, Vec<char>)> = (0..vocab.len())
            .filter(|&id| id != vocab.blank())
            .filter_map(|id| {
                let token = vocab.token(id)?;
                let chars = lower(token);
                (!chars.is_empty() && !token.starts_with('<')).then_some((id, chars))
            })
            .collect();
        let pieces = phrases
            .iter()
            .map(|phrase| {
                (0..phrase.len())
                    .map(|offset| {
                        let rest = &phrase[offset..];
                        tokens
                            .iter()
                            .filter(|(_, chars)| rest.starts_with(chars))
                            .map(|(id, chars)| (*id, chars.len()))
                            .collect()
                    })
                    .collect()
            })
            .collect();
        let bonus = phrases
            .iter()
            .map(|p| {
                let letters = p.iter().filter(|c| c.is_alphanumeric()).count();
                if letters < SHORT_PHRASE { bonus / 2.0 } else { bonus }
            })
            .collect();
        Some(Self { bonus, lengths: phrases.iter().map(Vec::len).collect(), pieces })
    }

    /// Picks the next token: the plain argmax, unless a boosted token beats it. `starts`
    /// allows boosting the start of a new phrase, not just the ones in progress.
    pub(crate) fn pick(&self, logits: &[f32], plain: usize, partials: &[Partial], starts: bool) -> usize {
        let mut best = plain;
        let mut best_score = logits[plain];
        let starts = (0..self.pieces.len()).filter(|_| starts).map(|phrase| (phrase, 0));
        let continuations = partials.iter().map(|p| (p.phrase, p.offset));
        for (phrase, offset) in starts.chain(continuations) {
            for &(id, _) in &self.pieces[phrase][offset] {
                let score = logits[id] + self.bonus[phrase];
                if score > best_score {
                    best = id;
                    best_score = score;
                }
            }
        }
        best
    }

    /// Updates the phrases in progress after `token` was emitted; returns whether it completed
    /// a phrase. `starts` lets the token begin new phrases.
    pub(crate) fn advance(&self, partials: &mut Vec<Partial>, token: usize, starts: bool) -> bool {
        let mut next = Vec::new();
        let mut completed = false;
        let mut step = |phrase: usize, offset: usize| {
            for &(id, len) in &self.pieces[phrase][offset] {
                let partial = Partial { phrase, offset: offset + len };
                if id != token {
                    continue;
                }
                if partial.offset == self.lengths[phrase] {
                    completed = true;
                } else if !next.contains(&partial) {
                    next.push(partial);
                }
            }
        };
        for p in partials.iter() {
            step(p.phrase, p.offset);
        }
        for phrase in (0..self.pieces.len()).filter(|_| starts) {
            step(phrase, 0);
        }
        *partials = next;
        completed
    }
}

/// "Claude  Code" -> "▁claude▁code"; `None` if there is nothing to match.
fn normalize(phrase: &str) -> Option<Vec<char>> {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    Some(lower(&format!("{WORD_START}{}", words.join(&WORD_START.to_string()))))
}

fn lower(s: &str) -> Vec<char> {
    s.chars().flat_map(char::to_lowercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab() -> Vocab {
        let tokens = ["<unk>", "▁ver", "▁vers", "cel", "al", "▁Ver", "▁the", "▁cl", "aude", "▁code", "<blk>"];
        let text: String = tokens.iter().enumerate().map(|(i, t)| format!("{t} {i}\n")).collect();
        Vocab::parse(&text).unwrap()
    }

    fn booster(phrases: &[&str]) -> Booster {
        let phrases: Vec<String> = phrases.iter().map(|s| s.to_string()).collect();
        Booster::new(&phrases, &vocab(), 3.0).unwrap()
    }

    #[test]
    fn empty_vocabulary_disables_boosting() {
        assert!(Booster::new(&[], &vocab(), 3.0).is_none());
        assert!(Booster::new(&["  ".to_string()], &vocab(), 3.0).is_none());
    }

    #[test]
    fn boosts_phrase_start_in_any_case() {
        let b = booster(&["Vercel"]);
        // "▁vers" (2) is the model's choice, "▁ver" (1) and "▁Ver" (5) start the phrase.
        let mut logits = vec![0.0; 11];
        logits[2] = 5.0;
        logits[1] = 3.0;
        assert_eq!(b.pick(&logits, 2, &[], true), 1);
        // A confident model is not overruled.
        logits[2] = 9.0;
        assert_eq!(b.pick(&logits, 2, &[], true), 2);
    }

    #[test]
    fn short_phrases_get_half_the_bonus() {
        let b = booster(&["ver"]);
        // Bonus 3.0 halved: "▁ver" (1) overtakes "▁vers" (2) by up to 1.5.
        let mut logits = vec![0.0; 11];
        logits[2] = 1.0;
        assert_eq!(b.pick(&logits, 2, &[], true), 1);
        logits[2] = 2.0;
        assert_eq!(b.pick(&logits, 2, &[], true), 2);
    }

    #[test]
    fn boosts_continuations_only_while_in_a_phrase() {
        let b = booster(&["Vercel"]);
        let mut partials = Vec::new();
        let mut logits = vec![0.0; 11];
        logits[4] = 4.0; // "al"
        logits[3] = 2.0; // "cel"
        assert_eq!(b.pick(&logits, 4, &partials, true), 4);
        b.advance(&mut partials, 1, true); // "▁ver"
        assert_eq!(partials, vec![Partial { phrase: 0, offset: 4 }]);
        assert_eq!(b.pick(&logits, 4, &partials, true), 3);
        assert!(b.advance(&mut partials, 3, true)); // "cel" completes the phrase
        assert!(partials.is_empty());
    }

    #[test]
    fn unrelated_token_drops_progress() {
        let b = booster(&["Claude Code"]);
        let mut partials = Vec::new();
        b.advance(&mut partials, 7, true); // "▁cl"
        assert_eq!(partials.len(), 1);
        assert!(!b.advance(&mut partials, 6, true)); // "▁the"
        assert!(partials.is_empty());
        for token in [7, 8] {
            b.advance(&mut partials, token, true);
        }
        assert_eq!(partials, vec![Partial { phrase: 0, offset: 7 }]);
        let mut logits = vec![0.0; 11];
        logits[6] = 2.0;
        logits[7] = -5.0;
        assert_eq!(b.pick(&logits, 6, &partials, true), 9); // "▁code"
    }
}
