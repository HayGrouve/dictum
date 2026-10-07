//! Deterministic clean-up of recognised text. No LLM, no network: microseconds, not seconds.

#[derive(Debug, Clone, Default)]
pub struct TextOptions {
    /// Drop hesitation sounds ("um", "uh", ...).
    pub remove_fillers: bool,
    /// Turn spoken "new line" / "new paragraph" into line breaks.
    pub voice_commands: bool,
    /// Terms to write exactly as listed ("next.js", "turbo repo", "Shadn" -> "Next.js",
    /// "Turborepo", "shadcn"). The same list boosts recognition in the engine.
    pub vocabulary: Vec<String>,
    /// Case-insensitive whole-phrase replacements, applied in order (personal dictionary).
    pub replacements: Vec<(String, String)>,
}

const FILLERS: &[&str] = &["um", "umm", "uh", "uhh", "uhm", "erm", "hmm", "mm"];

/// Joins the transcripts of consecutive segments of one dictation.
pub fn join_segments<I: IntoIterator<Item = String>>(parts: I) -> String {
    let mut out = String::new();
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(part);
    }
    out
}

pub fn process(text: &str, options: &TextOptions) -> String {
    let mut text = text.trim().to_string();
    if options.remove_fillers {
        text = remove_fillers(&text);
    }
    if options.voice_commands {
        text = apply_voice_commands(&text);
    }
    if !options.vocabulary.is_empty() {
        text = apply_vocabulary(&text, &options.vocabulary);
    }
    for (from, to) in &options.replacements {
        text = replace_phrase(&text, from, to);
    }
    text
}

/// A word with the punctuation glued to it, e.g. `"Um,"` -> ("", "Um", ",").
struct Word<'a> {
    lead: &'a str,
    core: &'a str,
    trail: &'a str,
}

fn split_word(raw: &str) -> Word<'_> {
    let start = raw.find(|c: char| c.is_alphanumeric()).unwrap_or(raw.len());
    let end = raw
        .rfind(|c: char| c.is_alphanumeric())
        .map(|i| i + raw[i..].chars().next().map_or(1, char::len_utf8))
        .unwrap_or(start);
    Word { lead: &raw[..start], core: &raw[start..end.max(start)], trail: &raw[end.max(start)..] }
}

fn ends_sentence(s: &str) -> bool {
    s.ends_with(['.', '!', '?', '\n'])
}

fn remove_fillers(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut capitalize_next = false;
    for raw in text.split(' ').filter(|w| !w.is_empty()) {
        let word = split_word(raw);
        let is_filler = FILLERS.iter().any(|f| word.core.eq_ignore_ascii_case(f));
        if !is_filler {
            let mut w = raw.to_string();
            if capitalize_next {
                w = capitalize_first(&w);
                capitalize_next = false;
            }
            out.push(w);
            continue;
        }
        let at_sentence_start = out.last().is_none_or(|prev| ends_sentence(prev));
        let ending = word.trail.trim_start_matches(',');
        if ends_sentence(ending) {
            // "I think, um." -> "I think."
            if let Some(prev) = out.last_mut() {
                let trimmed = prev.trim_end_matches([',', ';', ':']).to_string();
                *prev = if ends_sentence(&trimmed) { trimmed } else { trimmed + ending };
            }
        } else if at_sentence_start {
            capitalize_next = word.core.starts_with(char::is_uppercase) || out.is_empty();
        }
    }
    out.join(" ")
}

/// Longest run of words considered for one vocabulary term ("p n p m" -> "pnpm").
const MAX_TERM_WORDS: usize = 6;
/// Terms at least this long (letters and digits) also match with one letter missing or extra
/// inside the word ("Shadn" -> "shadcn").
const FUZZY_MIN_LEN: usize = 6;

/// Rewrites vocabulary terms the way they are listed. A run of words matches a term when their
/// letters and digits agree ignoring case, spaces and punctuation, or for long terms, when they
/// differ by one letter inserted or dropped inside the word.
fn apply_vocabulary(text: &str, vocabulary: &[String]) -> String {
    let terms: Vec<(&str, Vec<char>)> = vocabulary
        .iter()
        .map(|t| t.trim())
        .map(|t| (t, compact(t)))
        .filter(|(_, key)| !key.is_empty())
        .collect();
    let words: Vec<&str> = text.split(' ').collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        match match_term(&words[i..], &terms) {
            Some((n, term, possessive)) => {
                let (first, last) = (split_word(words[i]), split_word(words[i + n - 1]));
                out.push(format!("{}{term}{possessive}{}", first.lead, last.trail));
                i += n;
            }
            None => {
                out.push(words[i].to_string());
                i += 1;
            }
        }
    }
    out.join(" ")
}

/// Finds the longest run of words at the start of `words` that spells a term; returns the
/// number of words, the term and a possessive suffix to keep.
fn match_term<'t>(words: &[&str], terms: &[(&'t str, Vec<char>)]) -> Option<(usize, &'t str, &'static str)> {
    let mut heard = Vec::new();
    let mut runs = Vec::new();
    for (n, raw) in words.iter().take(MAX_TERM_WORDS).enumerate() {
        let word = split_word(raw);
        if word.core.is_empty() || (n > 0 && !word.lead.is_empty()) {
            break;
        }
        heard.extend(compact(word.core));
        runs.push(heard.clone());
        if !word.trail.is_empty() {
            break; // punctuation ends the run
        }
    }
    for (n, heard) in runs.iter().enumerate().rev() {
        let (stem, possessive) = match heard.len().checked_sub(2) {
            Some(cut) if heard[cut..] == ['\'', 's'] => (&heard[..cut], "'s"),
            _ => (&heard[..], ""),
        };
        let stem: Vec<char> = stem.iter().copied().filter(|c| c.is_alphanumeric()).collect();
        let exact = terms.iter().find(|(_, key)| *key == stem);
        let fuzzy =
            || terms.iter().find(|(_, key)| key.len() >= FUZZY_MIN_LEN && one_letter_apart(key, &stem));
        if let Some((term, _)) = exact.or_else(fuzzy) {
            return Some((n + 1, term, possessive));
        }
    }
    None
}

/// Lower-case letters and digits (plus apostrophes, to recognise possessives).
fn compact(text: &str) -> Vec<char> {
    text.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '\'' | '’'))
        .flat_map(char::to_lowercase)
        .map(|c| if c == '’' { '\'' } else { c })
        .collect()
}

/// True when one is the other with a single letter inserted somewhere other than the first or
/// last position (so plurals and other endings never match).
fn one_letter_apart(a: &[char], b: &[char]) -> bool {
    let (long, short) = if a.len() > b.len() { (a, b) } else { (b, a) };
    if long.len() != short.len() + 1 || short.len() < 2 {
        return false;
    }
    (1..long.len() - 1).any(|k| long[..k] == short[..k] && long[k + 1..] == short[k..])
}

fn capitalize_first(word: &str) -> String {
    let split = split_word(word);
    let mut chars = split.core.chars();
    match chars.next() {
        Some(first) => format!("{}{}{}{}", split.lead, first.to_uppercase(), chars.as_str(), split.trail),
        None => word.to_string(),
    }
}

fn apply_voice_commands(text: &str) -> String {
    let mut out = text.to_string();
    for (phrase, replacement) in [("new paragraph", "\n\n"), ("new line", "\n")] {
        out = replace_command(&out, phrase, replacement);
    }
    out
}

/// Replaces a spoken command and the punctuation the model put around it.
fn replace_command(text: &str, phrase: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut capitalize = false;
    while let Some(pos) = find_phrase(rest, phrase) {
        let before = rest[..pos].trim_end_matches([' ', ',']);
        push_text(&mut out, before, capitalize);
        let after = rest[pos + phrase.len()..].trim_start_matches(['.', ',', '!', '?', ';', ':', ' ']);
        out.truncate(out.trim_end_matches(' ').len());
        out.push_str(replacement);
        rest = after;
        capitalize = true;
    }
    push_text(&mut out, rest, capitalize);
    out
}

fn push_text(out: &mut String, text: &str, capitalize: bool) {
    if capitalize {
        let mut chars = text.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
            return;
        }
    }
    out.push_str(text);
}

/// Case-insensitive search for `phrase` at word boundaries; returns a byte offset into `text`.
fn find_phrase(text: &str, phrase: &str) -> Option<usize> {
    if phrase.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    let needle = phrase.to_lowercase();
    // Lower-casing can change byte lengths for some scripts; only use offsets when it didn't.
    if lower.len() != text.len() {
        return None;
    }
    let mut from = 0;
    while let Some(found) = lower[from..].find(&needle) {
        let start = from + found;
        let end = start + needle.len();
        let before_ok = lower[..start].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let after_ok = lower[end..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        if before_ok && after_ok {
            return Some(start);
        }
        from = start + needle[..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn replace_phrase(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = find_phrase(rest, from) {
        out.push_str(&rest[..pos]);
        out.push_str(to);
        rest = &rest[pos + from.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fillers() -> TextOptions {
        TextOptions { remove_fillers: true, ..Default::default() }
    }

    #[test]
    fn removes_leading_filler_and_capitalises() {
        assert_eq!(process("Um, so I was thinking.", &fillers()), "So I was thinking.");
        assert_eq!(process("Uh so I was thinking.", &fillers()), "So I was thinking.");
    }

    #[test]
    fn removes_inner_fillers() {
        assert_eq!(
            process("I think, um, we should go, uh, tomorrow.", &fillers()),
            "I think, we should go, tomorrow."
        );
        assert_eq!(process("Send it to, uhm. Mark.", &fillers()), "Send it to. Mark.");
    }

    #[test]
    fn filler_before_full_stop_moves_the_stop() {
        assert_eq!(process("I think, um.", &fillers()), "I think.");
        assert_eq!(process("Done. Um. Next.", &fillers()), "Done. Next.");
    }

    #[test]
    fn keeps_real_words() {
        let t = "Umbrella and hummus are fine, mmm no.";
        assert_eq!(process(t, &fillers()), t);
    }

    #[test]
    fn only_fillers_becomes_empty() {
        assert_eq!(process("Um.", &fillers()), "");
        assert_eq!(process("Uh, um.", &fillers()), "");
    }

    #[test]
    fn voice_commands_insert_breaks() {
        let o = TextOptions { voice_commands: true, ..Default::default() };
        assert_eq!(process("Hello. New line. how are you?", &o), "Hello.\nHow are you?");
        assert_eq!(process("Dear Sam, new paragraph, thanks", &o), "Dear Sam\n\nThanks");
        assert_eq!(process("a newline here", &o), "a newline here");
    }

    #[test]
    fn replacements_are_case_insensitive_whole_words() {
        let o = TextOptions {
            replacements: vec![("dictum".into(), "Dictum".into()), ("get hub".into(), "GitHub".into())],
            ..Default::default()
        };
        assert_eq!(
            process("I pushed dictum to get hub, not dictums.", &o),
            "I pushed Dictum to GitHub, not dictums."
        );
    }

    fn vocabulary(terms: &[&str]) -> TextOptions {
        TextOptions { vocabulary: terms.iter().map(|t| t.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn vocabulary_fixes_case_and_spacing() {
        let o =
            vocabulary(&["Next.js", "Turborepo", "subagent", "TanStack Query", "TanStack", "AI SDK", "pnpm"]);
        assert_eq!(
            process("Use PNPM, turbo repo and the next.js app.", &o),
            "Use pnpm, Turborepo and the Next.js app."
        );
        assert_eq!(
            process("Let the Suba Gent use TANSTACK query.", &o),
            "Let the subagent use TanStack Query."
        );
        assert_eq!(
            process("Stream it with the AISDK and TanStack.", &o),
            "Stream it with the AI SDK and TanStack."
        );
    }

    #[test]
    fn vocabulary_keeps_punctuation_and_possessives() {
        let o = vocabulary(&["Claude Code", "Vercel"]);
        assert_eq!(process("(claude code's) docs, on VERCEL!", &o), "(Claude Code's) docs, on Vercel!");
        // Punctuation between words breaks a match.
        assert_eq!(process("Claude, code it.", &o), "Claude, code it.");
    }

    #[test]
    fn vocabulary_fuzzy_match_is_narrow() {
        let o = vocabulary(&["shadcn", "webhook", "Claude", "Convex"]);
        assert_eq!(process("Add it from Shadn.", &o), "Add it from shadcn.");
        // Endings, substitutions and short terms are left alone.
        assert_eq!(process("Two webhooks, a clause, convexs.", &o), "Two webhooks, a clause, convexs.");
        assert_eq!(process("A web hook", &o), "A webhook");
    }

    #[test]
    fn vocabulary_does_not_touch_other_words() {
        let o = vocabulary(&["Claude Code", "Vercel", "shadcn", "Convex", "pnpm", "Rust"]);
        let t = "The weather today is cloudy, with a chance of rain in the evening.";
        assert_eq!(process(t, &o), t);
    }

    #[test]
    fn non_ascii_text_passes_through() {
        let o = TextOptions { remove_fillers: true, voice_commands: true, ..Default::default() };
        assert_eq!(process("Здравей, свят!", &o), "Здравей, свят!");
    }

    #[test]
    fn joins_segments_with_single_spaces() {
        assert_eq!(join_segments(vec!["Hello.".into(), " ".into(), " world ".into()]), "Hello. world");
    }
}
