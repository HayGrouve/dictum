//! What the settings window shows and reads back, as plain text. The window itself is
//! platform-specific; the conversions and validation live here so they are tested everywhere.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use crate::hotkey::{self, Hotkey};

/// Offered in the hotkey drop-down; any other spec can still be typed in.
pub const HOTKEY_PRESETS: [&str; 5] = ["ctrl+win", "right_ctrl", "right_alt", "ctrl+shift+space", "f13"];

/// How a hotkey spec is shown: `right_ctrl` → `Right Ctrl`. Invalid specs are shown as written.
pub fn hotkey_label(spec: &str) -> String {
    Hotkey::parse(spec).map_or_else(|_| spec.to_string(), |h| h.to_string())
}

/// The "Controls" lines: how to dictate, go hands-free and cancel with the configured keys.
pub fn controls_help(hotkey: &str, hands_free_key: &str, cancel_key: &str) -> [String; 3] {
    let key = |spec: &str| hotkey::parse_key(spec).map_or_else(|_| spec.to_string(), hotkey::key_name);
    let hotkey = hotkey_label(hotkey);
    [
        format!("Hold {hotkey} and speak; release to insert the text."),
        format!(
            "While holding, tap {} to keep recording hands-free; press {hotkey} again to finish.",
            key(hands_free_key)
        ),
        format!("Press {} while recording to discard it.", key(cancel_key)),
    ]
}

/// Reads the hotkey field back into a spec. `current` is kept as written when the field still
/// means the same hotkey; presets are written the way they are listed.
pub fn hotkey_spec(text: &str, current: &str) -> Result<String> {
    let hotkey = Hotkey::parse(text).with_context(|| format!("“{}” is not a valid hotkey", text.trim()))?;
    if Hotkey::parse(current).is_ok_and(|c| c == hotkey) {
        return Ok(current.to_string());
    }
    if let Some(preset) = HOTKEY_PRESETS.iter().find(|p| Hotkey::parse(p).is_ok_and(|p| p == hotkey)) {
        return Ok(preset.to_string());
    }
    Ok(text.split('+').map(|p| p.trim().to_ascii_lowercase().replace(' ', "_")).collect::<Vec<_>>().join("+"))
}

/// Entries for the microphone drop-down as (label, setting), and the one to select. `devices` is
/// `None` until the device list has been read.
pub fn microphone_choices(current: &str, devices: Option<&[String]>) -> (Vec<(String, String)>, usize) {
    let mut choices = vec![("System default".to_string(), String::new())];
    if current.trim().is_empty() {
        choices.extend(devices.unwrap_or_default().iter().map(|d| (d.clone(), d.clone())));
        return (choices, 0);
    }
    let Some(devices) = devices else {
        choices.push((current.to_string(), current.to_string()));
        return (choices, 1);
    };
    choices.extend(devices.iter().map(|d| (d.clone(), d.clone())));
    // The device the setting picks (the first whose name contains it) keeps the setting as written.
    let wanted = current.trim().to_lowercase();
    match choices.iter().skip(1).position(|(name, _)| name.to_lowercase().contains(&wanted)) {
        Some(i) => {
            choices[i + 1].1 = current.to_string();
            (choices, i + 1)
        }
        None => {
            choices.push((format!("{current} (not connected)"), current.to_string()));
            let last = choices.len() - 1;
            (choices, last)
        }
    }
}

/// One term per line (CRLF, as Windows edit controls expect).
pub fn vocabulary_text(terms: &[String]) -> String {
    terms.join("\r\n")
}

/// One term per line; blank lines and repeats are dropped.
pub fn parse_vocabulary(text: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !terms.iter().any(|t| t == line) {
            terms.push(line.to_string());
        }
    }
    terms
}

/// One `heard = written` pair per line.
pub fn replacements_text(replacements: &BTreeMap<String, String>) -> String {
    replacements
        .iter()
        .map(|(heard, written)| format!("{heard} = {written}"))
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// Parses `heard = written` lines; blank lines are skipped and a later line wins over an earlier
/// one for the same phrase.
pub fn parse_replacements(text: &str) -> Result<BTreeMap<String, String>> {
    let mut replacements = BTreeMap::new();
    for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let Some((heard, written)) = line.split_once('=') else {
            bail!("replacement on line {} needs an “=”, as in: get hub = GitHub", i + 1);
        };
        let heard = heard.trim();
        if heard.is_empty() {
            bail!("replacement on line {} has nothing before the “=”", i + 1);
        }
        replacements.insert(heard.to_string(), written.trim().to_string());
    }
    Ok(replacements)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_round_trip() {
        assert_eq!(hotkey_label("right_ctrl"), "Right Ctrl");
        assert_eq!(hotkey_label("ctrl+banana"), "ctrl+banana");
        for spec in HOTKEY_PRESETS {
            assert_eq!(hotkey_spec(&hotkey_label(spec), "right_alt").unwrap(), spec);
        }
        // Unchanged hotkeys keep the user's spelling.
        assert_eq!(hotkey_spec("Ctrl+Win", "control+super").unwrap(), "control+super");
        assert_eq!(hotkey_spec(" Left Alt + Space ", "ctrl+win").unwrap(), "left_alt+space");
        assert_eq!(hotkey_spec("control+super", "f13").unwrap(), "ctrl+win");
        assert!(hotkey_spec("ctrl+banana", "ctrl+win").is_err());
        assert!(hotkey_spec("", "ctrl+win").is_err());
    }

    #[test]
    fn controls_help_names_the_keys() {
        let [hold, hands_free, cancel] = controls_help("right_ctrl", "space", "escape");
        assert_eq!(hold, "Hold Right Ctrl and speak; release to insert the text.");
        assert!(hands_free.contains("tap Space") && hands_free.contains("press Right Ctrl again"));
        assert_eq!(cancel, "Press Esc while recording to discard it.");
        let [_, hands_free, _] = controls_help("ctrl+win", "f9", "esc");
        assert!(hands_free.contains("tap F9") && hands_free.contains("press Ctrl+Win again"));
    }

    #[test]
    fn microphone_choices_keep_the_setting() {
        let devices = ["Microphone (Realtek Audio)".to_string(), "Headset (USB Audio)".to_string()];
        let (choices, selected) = microphone_choices("", Some(&devices));
        assert_eq!(choices.len(), 3);
        assert_eq!((choices[0].1.as_str(), selected), ("", 0));

        let (choices, selected) = microphone_choices("usb", Some(&devices));
        assert_eq!(selected, 2);
        assert_eq!(choices[2], ("Headset (USB Audio)".to_string(), "usb".to_string()));
        assert_eq!(choices[1].1, "Microphone (Realtek Audio)");

        let (choices, selected) = microphone_choices("Blue Yeti", Some(&devices));
        assert_eq!(choices[selected], ("Blue Yeti (not connected)".to_string(), "Blue Yeti".to_string()));

        let (choices, selected) = microphone_choices("Blue Yeti", None);
        assert_eq!(choices[selected], ("Blue Yeti".to_string(), "Blue Yeti".to_string()));
    }

    #[test]
    fn vocabulary_round_trip() {
        let terms = vec!["Claude Code".to_string(), "shadcn".to_string()];
        assert_eq!(parse_vocabulary(&vocabulary_text(&terms)), terms);
        assert_eq!(parse_vocabulary("  Vercel \r\n\r\nConvex\nVercel\n"), vec!["Vercel", "Convex"]);
        assert!(parse_vocabulary(" \r\n").is_empty());
    }

    #[test]
    fn replacements_round_trip() {
        let map: BTreeMap<String, String> =
            [("get hub", "GitHub"), ("versal", "Vercel")].map(|(a, b)| (a.into(), b.into())).into();
        assert_eq!(parse_replacements(&replacements_text(&map)).unwrap(), map);
        let parsed = parse_replacements("get hub=GitHub\r\n\r\n  um =  \nget hub = Github\n").unwrap();
        assert_eq!(parsed["get hub"], "Github");
        assert_eq!(parsed["um"], "");
        assert!(parse_replacements("ok = fine\nmissing equals").unwrap_err().to_string().contains("line 2"));
        assert!(parse_replacements(" = GitHub").is_err());
    }
}
