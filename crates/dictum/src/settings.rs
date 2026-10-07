//! The settings window, minus the drawing: which settings it shows and how, in what order, how
//! each one is read from and written back to the config, validation, and saving. Every platform
//! draws its window from this, so the windows stay the same everywhere and are tested here.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::config::{self, Config, InsertMethod};
use crate::hotkey::{self, Hotkey};

pub const TITLE: &str = "Dictum settings";

/// Offered in the hotkey drop-down; any other spec can still be typed in.
pub const HOTKEY_PRESETS: [&str; 5] = if cfg!(target_os = "macos") {
    ["fn", "right_cmd", "right_option", "ctrl+cmd", "ctrl+option+space"]
} else {
    ["ctrl+win", "right_ctrl", "right_alt", "ctrl+shift+space", "f13"]
};

const HOTKEY_HINT: &str = "Pick one from the list, or type keys joined by “+”, like ctrl+shift+space.";

/// The insert method choices, in [`InsertMethod`] order: paste, type.
const INSERT_METHODS: [&str; 2] =
    ["Pasting (fast; your clipboard is kept)", "Typing (never touches the clipboard)"];

const AUTOSTART: &str = if cfg!(windows) { "Start with Windows" } else { "Start at login" };

pub const CONTROLS_TITLE: &str = "Controls";
pub const CONTROLS_NOTE: &str = "The hands-free and cancel keys can be changed in the config file.";
pub const OPEN_FILE: &str = "&Open config file";
pub const SAVE_NOTE: &str = "Saving restarts Dictum to apply the changes.";
pub const SAVE: &str = "Save";
pub const CANCEL: &str = "Cancel";

/// Line breaks in multi-line fields: Windows edit controls want CRLF.
const NEWLINE: &str = if cfg!(windows) { "\r\n" } else { "\n" };

/// A titled group of fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Dictation,
    Text,
    Vocabulary,
}

impl Section {
    /// In the order the window shows them.
    pub const ALL: [Section; 3] = [Section::Dictation, Section::Text, Section::Vocabulary];

    pub fn title(self) -> &'static str {
        match self {
            Section::Dictation => "Dictation",
            Section::Text => "Text",
            Section::Vocabulary => "Vocabulary",
        }
    }

    pub fn fields(self) -> &'static [Field] {
        use Field::*;
        match self {
            Section::Dictation => &[Hotkey, Microphone, Indicator, Sounds, Autostart],
            Section::Text => {
                &[InsertMethod, RestoreClipboard, RemoveFillers, RemoveStutters, VoiceCommands, TrailingSpace]
            }
            Section::Vocabulary => &[Vocabulary, Replacements],
        }
    }
}

/// Every setting the window edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Hotkey,
    Microphone,
    Indicator,
    Sounds,
    Autostart,
    InsertMethod,
    RestoreClipboard,
    RemoveFillers,
    RemoveStutters,
    VoiceCommands,
    TrailingSpace,
    Vocabulary,
    Replacements,
}

/// How a field is edited. Labels mark their keyboard access key with `&`, as Windows does; use
/// [`plain`] where there are none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// A drop-down list. Editable ones also take typed text and are read back as [`Value::Text`],
    /// the others as [`Value::Choice`].
    Choice {
        label: &'static str,
        editable: bool,
    },
    Check {
        label: &'static str,
    },
    /// Multi-line text, explained by `label` above it.
    Lines {
        label: &'static str,
    },
}

impl Field {
    pub fn control(self) -> Control {
        let check = |label| Control::Check { label };
        match self {
            Field::Hotkey => Control::Choice { label: "&Hotkey:", editable: true },
            Field::Microphone => Control::Choice { label: "&Microphone:", editable: false },
            Field::Indicator => check("Show an indicator on screen while dictating"),
            Field::Sounds => check("Play a sound when recording starts and stops"),
            Field::Autostart => check(AUTOSTART),
            Field::InsertMethod => Control::Choice { label: "&Insert text by:", editable: false },
            Field::RestoreClipboard => check("Put the clipboard back after pasting"),
            Field::RemoveFillers => check("Remove filler words (um, uh)"),
            Field::RemoveStutters => check("Remove stutters (“I I I want” → “I want”)"),
            Field::VoiceCommands => check("Voice commands: “new line”, “new paragraph”"),
            Field::TrailingSpace => check("Add a space after each dictation"),
            Field::Vocabulary => Control::Lines {
                label: "&Words and names recognition tends to get wrong, one per line, spelled the way you \
                        want them:",
            },
            Field::Replacements => Control::Lines {
                label: "&Replacements for anything else, one per line:  heard\u{a0}=\u{a0}written",
            },
        }
    }
}

/// `label` without its access-key marker.
#[cfg_attr(windows, allow(dead_code))] // Windows shows access keys
pub fn plain(label: &str) -> String {
    label.replace('&', "")
}

/// What a field holds in the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Check(bool),
    /// The selected entry of a drop-down list.
    Choice(Option<usize>),
    Text(String),
}

impl Value {
    fn check(&self) -> bool {
        matches!(self, Value::Check(true))
    }

    fn choice(&self) -> Option<usize> {
        match self {
            Value::Choice(selected) => *selected,
            _ => None,
        }
    }

    fn text(&self) -> &str {
        match self {
            Value::Text(text) => text,
            _ => "",
        }
    }
}

/// What the user asked for, read from the window.
#[derive(Debug, Clone)]
pub struct Changes {
    pub config: Config,
    pub autostart: bool,
}

/// Why the settings can't be saved, and the field to fix (if it is one field's fault).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub field: Option<Field>,
    pub message: String,
}

impl Problem {
    fn new(field: impl Into<Option<Field>>, message: String) -> Self {
        Self { field: field.into(), message }
    }
}

/// The open window's data: the settings it started from and the entries behind its lists.
pub struct Form {
    /// The file's settings when the window opened: only fields that differ get written.
    original: Config,
    autostart: bool,
    /// The `microphone` setting behind each microphone entry.
    microphones: Vec<String>,
}

impl Form {
    pub fn new(original: Config, autostart: bool) -> Self {
        let (choices, _) = microphone_choices(&original.microphone, None);
        let microphones = choices.into_iter().map(|(_, setting)| setting).collect();
        Self { original, autostart, microphones }
    }

    /// The entries of a drop-down list.
    pub fn options(&self, field: Field) -> Vec<String> {
        match field {
            Field::Hotkey => HOTKEY_PRESETS.iter().map(|spec| hotkey_label(spec)).collect(),
            Field::Microphone => microphone_choices(&self.original.microphone, None)
                .0
                .into_iter()
                .map(|(label, _)| label)
                .collect(),
            Field::InsertMethod => INSERT_METHODS.map(String::from).to_vec(),
            _ => Vec::new(),
        }
    }

    /// What a field shows when the window opens.
    pub fn initial(&self, field: Field) -> Value {
        let c = &self.original;
        match field {
            Field::Hotkey => Value::Text(hotkey_label(&c.hotkey)),
            Field::Microphone => Value::Choice(Some(microphone_choices(&c.microphone, None).1)),
            Field::Indicator => Value::Check(c.indicator),
            Field::Sounds => Value::Check(c.sounds),
            Field::Autostart => Value::Check(self.autostart),
            Field::InsertMethod => Value::Choice(Some(match c.insert_method {
                InsertMethod::Paste => 0,
                InsertMethod::Type => 1,
            })),
            Field::RestoreClipboard => Value::Check(c.restore_clipboard),
            Field::RemoveFillers => Value::Check(c.remove_fillers),
            Field::RemoveStutters => Value::Check(c.remove_stutters),
            Field::VoiceCommands => Value::Check(c.voice_commands),
            Field::TrailingSpace => Value::Check(c.trailing_space),
            Field::Vocabulary => Value::Text(vocabulary_text(&c.vocabulary)),
            Field::Replacements => Value::Text(replacements_text(&c.replacements)),
        }
    }

    /// The connected microphones arrived: the new microphone entries and the one to select, given
    /// the entry selected now.
    pub fn devices_found(&mut self, devices: &[String], selected: Option<usize>) -> (Vec<String>, usize) {
        let current = selected.and_then(|i| self.microphones.get(i)).cloned().unwrap_or_default();
        let (choices, selected) = microphone_choices(&current, Some(devices));
        let (labels, values) = choices.into_iter().unzip();
        self.microphones = values;
        (labels, selected)
    }

    /// The "Controls" lines for the hotkey in the window; `None` while it is not a valid hotkey
    /// (still being typed), when the lines should stay as they are.
    pub fn controls(&self, hotkey: &str) -> Option<[String; 3]> {
        Hotkey::parse(hotkey).ok()?;
        Some(controls_help(hotkey, &self.original.hands_free_key, &self.original.cancel_key))
    }

    /// Whether a field can be changed, given what the window shows.
    pub fn enabled(field: Field, value: impl Fn(Field) -> Value) -> bool {
        match field {
            Field::RestoreClipboard => value(Field::InsertMethod).choice() == Some(0),
            _ => true,
        }
    }

    /// Reads the window back into settings.
    pub fn read(&self, value: impl Fn(Field) -> Value) -> Result<Changes, Problem> {
        let mut changes = Changes { config: self.original.clone(), autostart: self.autostart };
        for field in Section::ALL.iter().flat_map(|s| s.fields()) {
            self.apply(&mut changes, *field, &value(*field))?;
        }
        Ok(changes)
    }

    fn apply(&self, changes: &mut Changes, field: Field, value: &Value) -> Result<(), Problem> {
        let c = &mut changes.config;
        match field {
            Field::Hotkey => {
                c.hotkey = hotkey_spec(value.text(), &self.original.hotkey)
                    .map_err(|e| Problem::new(field, format!("{e:#}\n\n{HOTKEY_HINT}")))?;
            }
            Field::Microphone => {
                c.microphone =
                    value.choice().and_then(|i| self.microphones.get(i)).cloned().unwrap_or_default();
            }
            Field::Indicator => c.indicator = value.check(),
            Field::Sounds => c.sounds = value.check(),
            Field::Autostart => changes.autostart = value.check(),
            Field::InsertMethod => {
                c.insert_method =
                    if value.choice() == Some(1) { InsertMethod::Type } else { InsertMethod::Paste };
            }
            Field::RestoreClipboard => c.restore_clipboard = value.check(),
            Field::RemoveFillers => c.remove_fillers = value.check(),
            Field::RemoveStutters => c.remove_stutters = value.check(),
            Field::VoiceCommands => c.voice_commands = value.check(),
            Field::TrailingSpace => c.trailing_space = value.check(),
            Field::Vocabulary => c.vocabulary = parse_vocabulary(value.text()),
            Field::Replacements => {
                c.replacements =
                    parse_replacements(value.text()).map_err(|e| Problem::new(field, format!("{e:#}")))?;
            }
        }
        Ok(())
    }

    /// Applies the changes: start at login through `set_autostart`, the rest into the config
    /// file. Returns whether the file changed (Dictum then restarts to use it).
    pub fn save(
        &mut self,
        config_file: &Path,
        changes: &Changes,
        set_autostart: impl FnOnce(bool) -> Result<()>,
    ) -> Result<bool, Problem> {
        if changes.autostart != self.autostart {
            set_autostart(changes.autostart).map_err(|e| {
                log::error!("{e:#}");
                Problem::new(Field::Autostart, format!("Could not change “{AUTOSTART}”: {e:#}"))
            })?;
            self.autostart = changes.autostart;
        }
        let changed = config::save_changes(config_file, &self.original, &changes.config).map_err(|e| {
            log::error!("{e:#}");
            Problem::new(None, format!("Could not save the settings: {e:#}"))
        })?;
        if changed {
            log::info!("settings saved");
        }
        Ok(changed)
    }
}

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

/// One term per line.
pub fn vocabulary_text(terms: &[String]) -> String {
    terms.join(NEWLINE)
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
        .join(NEWLINE)
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
            assert_eq!(hotkey_spec(&hotkey_label(spec), "ctrl+k").unwrap(), spec);
        }
        // Unchanged hotkeys keep the user's spelling.
        assert_eq!(hotkey_spec("Ctrl+Win", "control+super").unwrap(), "control+super");
        assert_eq!(hotkey_spec(" Left Alt + Space ", "ctrl+win").unwrap(), "left_alt+space");
        let preset = if cfg!(target_os = "macos") { "ctrl+cmd" } else { "ctrl+win" };
        assert_eq!(hotkey_spec("control+super", "f13").unwrap(), preset);
        assert!(hotkey_spec("ctrl+banana", "ctrl+win").is_err());
        assert!(hotkey_spec("", "ctrl+win").is_err());
    }

    #[test]
    fn controls_help_names_the_keys() {
        let [hold, hands_free, cancel] = controls_help("right_ctrl", "space", "escape");
        assert_eq!(hold, "Hold Right Ctrl and speak; release to insert the text.");
        assert!(hands_free.contains("tap Space") && hands_free.contains("press Right Ctrl again"));
        assert_eq!(cancel, "Press Esc while recording to discard it.");
        let [_, hands_free, _] = controls_help("ctrl+shift+space", "f9", "esc");
        assert!(hands_free.contains("tap F9") && hands_free.contains("press Ctrl+Shift+Space again"));
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

    /// The window as a platform would show it: every field's initial value, editable.
    struct Window {
        values: Vec<(Field, Value)>,
    }

    impl Window {
        fn open(form: &Form) -> Self {
            let fields = Section::ALL.iter().flat_map(|s| s.fields());
            Self { values: fields.map(|&f| (f, form.initial(f))).collect() }
        }

        fn get(&self, field: Field) -> Value {
            self.values.iter().find(|(f, _)| *f == field).unwrap().1.clone()
        }

        fn set(&mut self, field: Field, value: Value) {
            self.values.iter_mut().find(|(f, _)| *f == field).unwrap().1 = value;
        }
    }

    fn sample() -> Config {
        Config::parse(
            "hotkey = \"right_ctrl\"\nvocabulary = [\"Vercel\", \"Claude Code\"]\n\
             [replacements]\n\"get hub\" = \"GitHub\"\n",
        )
        .unwrap()
    }

    #[test]
    fn every_field_appears_once() {
        let fields: Vec<Field> = Section::ALL.iter().flat_map(|s| s.fields()).copied().collect();
        for field in &fields {
            assert_eq!(fields.iter().filter(|f| *f == field).count(), 1, "{field:?}");
            let (Control::Choice { label, .. } | Control::Check { label } | Control::Lines { label }) =
                field.control();
            assert!(!plain(label).contains('&'));
        }
        assert_eq!(fields.len(), 13);
    }

    #[test]
    fn unchanged_window_reads_back_the_same_settings() {
        let original = sample();
        let form = Form::new(original.clone(), true);
        let window = Window::open(&form);
        assert_eq!(window.get(Field::Hotkey), Value::Text("Right Ctrl".into()));
        assert_eq!(window.get(Field::Microphone), Value::Choice(Some(0)));
        assert_eq!(form.options(Field::InsertMethod).len(), 2);
        assert_eq!(form.options(Field::Hotkey).len(), HOTKEY_PRESETS.len());

        let changes = form.read(|f| window.get(f)).unwrap();
        assert!(changes.autostart);
        let c = &changes.config;
        assert_eq!(c.hotkey, original.hotkey);
        assert_eq!(c.vocabulary, original.vocabulary);
        assert_eq!(c.replacements, original.replacements);
        assert_eq!(
            (c.sounds, c.indicator, c.insert_method),
            (original.sounds, original.indicator, original.insert_method)
        );
        assert_eq!(c.microphone, "");
    }

    #[test]
    fn edits_reach_the_settings() {
        let form = Form::new(sample(), false);
        let mut window = Window::open(&form);
        window.set(Field::Hotkey, Value::Text("Ctrl+Shift+Space".into()));
        window.set(Field::Sounds, Value::Check(false));
        window.set(Field::InsertMethod, Value::Choice(Some(1)));
        window.set(Field::Autostart, Value::Check(true));
        window.set(Field::Vocabulary, Value::Text("pnpm\nVercel\n\npnpm".into()));
        let changes = form.read(|f| window.get(f)).unwrap();
        assert_eq!(changes.config.hotkey, "ctrl+shift+space");
        assert!(!changes.config.sounds);
        assert_eq!(changes.config.insert_method, InsertMethod::Type);
        assert!(changes.autostart);
        assert_eq!(changes.config.vocabulary, vec!["pnpm", "Vercel"]);
    }

    #[test]
    fn problems_name_the_field() {
        let form = Form::new(sample(), false);
        let mut window = Window::open(&form);
        window.set(Field::Hotkey, Value::Text("ctrl+banana".into()));
        let problem = form.read(|f| window.get(f)).unwrap_err();
        assert_eq!(problem.field, Some(Field::Hotkey));
        assert!(problem.message.contains("ctrl+banana") && problem.message.contains(HOTKEY_HINT));

        window.set(Field::Hotkey, Value::Text("Right Ctrl".into()));
        window.set(Field::Replacements, Value::Text("no equals sign".into()));
        assert_eq!(form.read(|f| window.get(f)).unwrap_err().field, Some(Field::Replacements));
    }

    #[test]
    fn restore_clipboard_only_applies_to_pasting() {
        let form = Form::new(sample(), false);
        let mut window = Window::open(&form);
        assert!(Form::enabled(Field::RestoreClipboard, |f| window.get(f)));
        window.set(Field::InsertMethod, Value::Choice(Some(1)));
        assert!(!Form::enabled(Field::RestoreClipboard, |f| window.get(f)));
        assert!(Form::enabled(Field::Sounds, |f| window.get(f)));
    }

    #[test]
    fn found_microphones_keep_the_selection() {
        let mut form = Form::new(Config::parse("microphone = \"usb\"").unwrap(), false);
        assert_eq!(form.initial(Field::Microphone), Value::Choice(Some(1)));
        let devices = ["Built-in Microphone".to_string(), "Headset (USB Audio)".to_string()];
        let (labels, selected) = form.devices_found(&devices, Some(1));
        assert_eq!((labels.len(), selected), (3, 2));
        let mut window = Window::open(&form);
        window.set(Field::Microphone, Value::Choice(Some(selected)));
        assert_eq!(form.read(|f| window.get(f)).unwrap().config.microphone, "usb");
        window.set(Field::Microphone, Value::Choice(Some(1)));
        assert_eq!(form.read(|f| window.get(f)).unwrap().config.microphone, "Built-in Microphone");
    }

    #[test]
    fn controls_follow_the_hotkey() {
        let form = Form::new(sample(), false);
        assert!(form.controls("Right Ctrl").unwrap()[0].contains("Hold Right Ctrl"));
        assert!(form.controls("ctrl+").is_none(), "still being typed");
    }

    #[test]
    fn saving_writes_only_changes_and_sets_autostart() {
        let dir = std::env::temp_dir().join(format!("dictum-settings-save-{}", std::process::id()));
        let path = dir.join("config.toml");
        let _ = std::fs::remove_dir_all(&dir);
        let mut form = Form::new(Config::load_or_create(&path).unwrap(), false);
        let window = Window::open(&form);
        let mut changes = form.read(|f| window.get(f)).unwrap();
        assert_eq!(form.save(&path, &changes, |_| unreachable!("autostart unchanged")), Ok(false));

        changes.autostart = true;
        changes.config.sounds = false;
        let mut asked = None;
        let saved = form.save(&path, &changes, |on| {
            asked = Some(on);
            Ok(())
        });
        assert_eq!(saved, Ok(true));
        assert_eq!(asked, Some(true));
        assert!(!Config::load_or_create(&path).unwrap().sounds);

        let failed = form.save(&path, &Changes { autostart: false, ..changes }, |_| bail!("denied"));
        assert_eq!(failed.unwrap_err().field, Some(Field::Autostart));
        std::fs::remove_dir_all(&dir).unwrap();
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
