//! Deterministic clean-up of recognised text. No LLM, no network: microseconds, not seconds.

#[derive(Debug, Clone, Default)]
pub struct TextOptions {
    /// Drop hesitation sounds ("um", "uh", ...).
    pub remove_fillers: bool,
    /// Drop stuttered repeats ("I I I want to" -> "I want to", "w- want" -> "want").
    pub remove_stutters: bool,
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
    if options.remove_stutters {
        text = remove_stutters(&text);
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

/// Longest phrase collapsed when repeated ("in the in the house" -> "in the house").
const MAX_STUTTER_WORDS: usize = 3;
/// Words people double on purpose ("I had had enough", "very very good"); three or more in a row
/// still collapse.
const DOUBLED_ON_PURPOSE: &[&str] =
    &["had", "that", "is", "do", "very", "really", "so", "no", "yes", "yeah", "bye", "ha", "knock"];

/// Collapses a word or short phrase said several times in a row into one, and drops cut-off
/// starts of the next word ("w- want"). Repeats across a full stop, or with digits, are kept.
fn remove_stutters(text: &str) -> String {
    // Repeats can nest ("the, the plan... the plan"): collapse until nothing changes.
    let mut text = text.to_string();
    loop {
        let next = collapse_stutters(&text);
        if next == text {
            return text;
        }
        text = next;
    }
}

fn collapse_stutters(text: &str) -> String {
    let words: Vec<&str> = text.split(' ').filter(|w| !w.is_empty()).collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut capitalize_next = false;
    let mut i = 0;
    while i < words.len() {
        if let Some(next) = words.get(i + 1).filter(|next| is_cut_off(words[i], next)) {
            // "W- want" -> "Want"
            capitalize_next |= split_word(words[i]).core.starts_with(char::is_uppercase)
                && !split_word(next).core.starts_with(char::is_uppercase);
            i += 1;
            continue;
        }
        let (n, copies) = (1..=MAX_STUTTER_WORDS)
            .map(|n| (n, repeats(&words[i..], n)))
            .find(|&(_, copies)| copies > 1)
            .unwrap_or((1, 1));
        for (k, raw) in words[i..i + n].iter().enumerate() {
            let mut w = raw.to_string();
            if k == n - 1 && copies > 1 {
                // Keep the first copy, ending with the punctuation of the last one.
                let first = split_word(raw);
                let last = split_word(words[i + n * copies - 1]);
                w = format!("{}{}{}", first.lead, first.core, last.trail);
            }
            if std::mem::take(&mut capitalize_next) {
                w = capitalize_first(&w);
            }
            out.push(w);
        }
        i += n * copies;
    }
    out.join(" ")
}

/// How many times the first `n` words repeat back to back (1 when they don't).
fn repeats(words: &[&str], n: usize) -> usize {
    let same = |a: &str, b: &str| {
        let (a, b) = (split_word(a), split_word(b));
        !b.core.is_empty()
            && !b.core.contains(|c: char| c.is_ascii_digit())
            && b.lead.is_empty()
            && a.core.to_lowercase() == b.core.to_lowercase()
    };
    let mut copies = 1;
    while let Some(next) = words.get(n * copies..n * (copies + 1)) {
        let previous = &words[n * (copies - 1)..n * copies];
        // "Go. Go." is two sentences, not a stutter.
        if !is_pause(split_word(previous[n - 1]).trail) || !previous.iter().zip(next).all(|(a, b)| same(a, b))
        {
            break;
        }
        copies += 1;
    }
    let word = split_word(words[0]).core.to_lowercase();
    if n == 1 && copies == 2 && DOUBLED_ON_PURPOSE.contains(&word.as_str()) {
        return 1;
    }
    copies
}

/// Punctuation that can sit between stuttered words: none, a comma, a dash or an ellipsis.
fn is_pause(trail: &str) -> bool {
    trail.is_empty() || trail == "..." || trail.chars().all(|c| matches!(c, ',' | '-' | '–' | '—' | '…'))
}

/// A word broken off with a dash that the next word completes: "w-" before "want".
fn is_cut_off(raw: &str, next: &str) -> bool {
    let (word, next) = (split_word(raw), split_word(next));
    word.lead.is_empty()
        && matches!(word.trail, "-" | "–" | "—")
        && !word.core.is_empty()
        && next.lead.is_empty()
        && next.core.to_lowercase().starts_with(&word.core.to_lowercase())
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

    fn stutters() -> TextOptions {
        TextOptions { remove_stutters: true, ..Default::default() }
    }

    #[test]
    fn collapses_repeated_words() {
        assert_eq!(process("I I I want to go.", &stutters()), "I want to go.");
        assert_eq!(process("I, I, I want to go.", &stutters()), "I want to go.");
        assert_eq!(process("So the the the plan is fine.", &stutters()), "So the plan is fine.");
        assert_eq!(process("The, the plan... the plan works.", &stutters()), "The plan works.");
        assert_eq!(process("It works, it works.", &stutters()), "It works.");
    }

    #[test]
    fn collapses_repeated_phrases() {
        assert_eq!(process("I want, I want to go.", &stutters()), "I want to go.");
        assert_eq!(process("Put it in the in the box.", &stutters()), "Put it in the box.");
    }

    #[test]
    fn drops_cut_off_words() {
        assert_eq!(process("I w- want to go.", &stutters()), "I want to go.");
        assert_eq!(process("W- we should go.", &stutters()), "We should go.");
        assert_eq!(process("I- I think so.", &stutters()), "I think so.");
        // Not a cut-off: the next word doesn't continue it.
        assert_eq!(process("Pre- and post-war.", &stutters()), "Pre- and post-war.");
    }

    #[test]
    fn keeps_deliberate_repeats() {
        for t in [
            "I had had enough.",
            "That is very very good.",
            "I know that that is true.",
            "No. No.",
            "Go. Go!",
            "Call 5 5 5 now.",
            "Bye bye.",
            "He said \"go\" go.",
        ] {
            assert_eq!(process(t, &stutters()), t);
        }
        assert_eq!(process("No no no, not that.", &stutters()), "No, not that.");
    }

    #[test]
    fn stutters_and_fillers_together() {
        let o = TextOptions { remove_fillers: true, remove_stutters: true, ..Default::default() };
        assert_eq!(process("I, um, I want to, uh, to go.", &o), "I want to go.");
        assert_eq!(process("Um, I I want it.", &o), "I want it.");
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
        let o = TextOptions {
            remove_fillers: true,
            remove_stutters: true,
            voice_commands: true,
            ..Default::default()
        };
        assert_eq!(process("Здравей, свят!", &o), "Здравей, свят!");
        assert_eq!(process("Аз аз искам това.", &o), "Аз искам това.");
    }

    #[test]
    fn joins_segments_with_single_spaces() {
        assert_eq!(join_segments(vec!["Hello.".into(), " ".into(), " world ".into()]), "Hello. world");
    }
}
