//! User settings: a commented TOML file, created with defaults on first run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use toml_edit::{Array, DocumentMut, Item, Table, Value};

use crate::hotkey::{self, Hotkey, Key};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InsertMethod {
    /// Put the text on the clipboard and press Ctrl+V / Cmd+V (fast, works almost everywhere).
    Paste,
    /// Type the text as synthetic key presses (never touches the clipboard).
    Type,
}

impl InsertMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            InsertMethod::Paste => "paste",
            InsertMethod::Type => "type",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceSetting {
    Cpu,
    Gpu,
}

/// The second hotkey: dictations rewritten by Claude into a clean coding prompt.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct PolishConfig {
    /// Empty: no polish hotkey.
    pub hotkey: String,
    /// Anthropic API key; empty means the `ANTHROPIC_API_KEY` environment variable.
    pub api_key: String,
    pub model: String,
    /// After this long past the release, the unpolished text is inserted instead.
    pub timeout_ms: u64,
    /// Added to the built-in instructions, e.g. the project's stack or preferred structure.
    pub instructions: String,
}

impl Default for PolishConfig {
    fn default() -> Self {
        Self {
            hotkey: String::new(),
            api_key: String::new(),
            model: "claude-haiku-5-5".to_string(),
            timeout_ms: 4000,
            instructions: String::new(),
        }
    }
}

/// What the Console lists for each key ("apikey_01…"); the key itself starts with "sk-ant-".
pub const KEY_ID_PREFIX: &str = "apikey_";

/// Why a pasted API key can't be right.
pub const KEY_ID_HINT: &str = "That is the key's ID from the Console's list of keys, not the key \
    itself. An API key starts with “sk-ant-”; the Console shows it only once, when you create it.";

impl PolishConfig {
    /// The configured key, or `ANTHROPIC_API_KEY` from the environment.
    pub fn api_key(&self) -> Option<String> {
        let key = self.api_key.trim();
        if !key.is_empty() {
            return Some(key.to_string());
        }
        std::env::var("ANTHROPIC_API_KEY").ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hotkey: String,
    pub hands_free_key: String,
    pub cancel_key: String,
    pub insert_method: InsertMethod,
    pub restore_clipboard: bool,
    pub trailing_space: bool,
    pub remove_fillers: bool,
    pub remove_stutters: bool,
    pub voice_commands: bool,
    pub sounds: bool,
    pub indicator: bool,
    pub microphone: String,
    pub device: DeviceSetting,
    pub threads: usize,
    pub model_dir: Option<PathBuf>,
    pub max_recording_secs: u32,
    pub vocabulary: Vec<String>,
    pub replacements: BTreeMap<String, String>,
    pub polish: PolishConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: if cfg!(target_os = "macos") { "fn" } else { "ctrl+win" }.to_string(),
            hands_free_key: "space".to_string(),
            cancel_key: "escape".to_string(),
            insert_method: InsertMethod::Paste,
            restore_clipboard: true,
            trailing_space: true,
            remove_fillers: true,
            remove_stutters: true,
            voice_commands: false,
            sounds: true,
            indicator: true,
            microphone: String::new(),
            device: DeviceSetting::Cpu,
            threads: 0,
            model_dir: None,
            max_recording_secs: 600,
            vocabulary: Vec::new(),
            replacements: BTreeMap::new(),
            polish: PolishConfig::default(),
        }
    }
}

/// Validated, ready-to-use key bindings.
#[derive(Debug, Clone)]
pub struct Bindings {
    pub hotkey: Hotkey,
    /// The hotkey for polished dictations, if one is set.
    pub polish: Option<Hotkey>,
    pub hands_free: Key,
    pub cancel: Key,
}

impl Config {
    pub fn bindings(&self) -> Result<Bindings> {
        let hotkey = Hotkey::parse(&self.hotkey).context("invalid `hotkey`")?;
        let hands_free = hotkey::parse_key(&self.hands_free_key).context("invalid `hands_free_key`")?;
        let cancel = hotkey::parse_key(&self.cancel_key).context("invalid `cancel_key`")?;
        let polish = match self.polish.hotkey.trim() {
            "" => None,
            spec => Some(Hotkey::parse(spec).context("invalid `polish.hotkey`")?),
        };
        if polish.as_ref() == Some(&hotkey) {
            bail!("the polish hotkey is the same as the dictation hotkey");
        }
        if polish.as_ref().is_some_and(|p| p.contains(hands_free) || p.contains(cancel)) {
            bail!("the polish hotkey can't use the hands-free or cancel key");
        }
        Ok(Bindings { hotkey, polish, hands_free, cancel })
    }

    pub fn text_options(&self) -> dictum_engine::text::TextOptions {
        dictum_engine::text::TextOptions {
            remove_fillers: self.remove_fillers,
            remove_stutters: self.remove_stutters,
            voice_commands: self.voice_commands,
            vocabulary: self.vocabulary.clone(),
            replacements: self.replacements.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        }
    }

    pub fn engine_options(&self) -> dictum_engine::EngineOptions {
        dictum_engine::EngineOptions {
            device: match self.device {
                DeviceSetting::Cpu => dictum_engine::Device::Cpu,
                DeviceSetting::Gpu => dictum_engine::Device::Gpu,
            },
            threads: self.threads,
            vocabulary: self.vocabulary.clone(),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text)?;
        config.bindings()?;
        Ok(config)
    }

    /// Loads the config, writing the commented default file if there is none.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("error in {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(path, default_file())
                    .with_context(|| format!("failed to write {}", path.display()))?;
                Ok(Self::default())
            }
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }
}

/// Writes the settings that differ between `from` and `to` into the config file, keeping its
/// comments, layout and every other setting. Returns whether the file changed.
pub fn save_changes(path: &Path, from: &Config, to: &Config) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => default_file(),
        Err(e) => return Err(e).with_context(|| format!("failed to read {}", path.display())),
    };
    let updated = edit(&text, from, to).with_context(|| format!("error in {}", path.display()))?;
    if updated == text {
        return Ok(false);
    }
    Config::parse(&updated).context("the new settings are invalid")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write then rename, so a crash can't leave a half-written file behind.
    let temp = path.with_extension("toml.tmp");
    std::fs::write(&temp, &updated).with_context(|| format!("failed to write {}", temp.display()))?;
    std::fs::rename(&temp, path).with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(true)
}

/// The settings window's fields, written into `text` where they changed.
fn edit(text: &str, from: &Config, to: &Config) -> Result<String> {
    let mut doc: DocumentMut = text.parse()?;
    let root = doc.as_table_mut();
    let mut set = |key: &str, changed: bool, value: Value| {
        if changed {
            set_value(root, key, value);
        }
    };
    set("hotkey", from.hotkey != to.hotkey, to.hotkey.as_str().into());
    set("insert_method", from.insert_method != to.insert_method, to.insert_method.as_str().into());
    set("restore_clipboard", from.restore_clipboard != to.restore_clipboard, to.restore_clipboard.into());
    set("trailing_space", from.trailing_space != to.trailing_space, to.trailing_space.into());
    set("remove_fillers", from.remove_fillers != to.remove_fillers, to.remove_fillers.into());
    set("remove_stutters", from.remove_stutters != to.remove_stutters, to.remove_stutters.into());
    set("voice_commands", from.voice_commands != to.voice_commands, to.voice_commands.into());
    set("sounds", from.sounds != to.sounds, to.sounds.into());
    set("indicator", from.indicator != to.indicator, to.indicator.into());
    set("microphone", from.microphone != to.microphone, to.microphone.as_str().into());
    set("vocabulary", from.vocabulary != to.vocabulary, string_array(&to.vocabulary));

    if from.replacements != to.replacements {
        let item = root.entry("replacements").or_insert_with(toml_edit::table);
        let Some(table) = item.as_table_like_mut() else { bail!("`replacements` is not a table") };
        let stale: Vec<String> =
            table.iter().map(|(k, _)| k.to_string()).filter(|k| !to.replacements.contains_key(k)).collect();
        for key in stale {
            table.remove(&key);
        }
        for (heard, written) in &to.replacements {
            if table.get(heard).and_then(Item::as_str) != Some(written) {
                table.insert(heard, toml_edit::value(written));
            }
        }
    }

    if from.polish.hotkey != to.polish.hotkey || from.polish.api_key != to.polish.api_key {
        // Files from before the polish hotkey end with `[replacements]` and a commented example:
        // a new table goes after that comment, not between it and its table.
        let trailing = doc.trailing().as_str().unwrap_or_default().to_string();
        if !doc.contains_key("polish") {
            let mut table = Table::new();
            table.decor_mut().set_prefix(format!("{trailing}\n"));
            doc.insert("polish", Item::Table(table));
            doc.set_trailing("");
        }
        let Some(table) = doc["polish"].as_table_mut() else { bail!("`polish` is not a table") };
        if from.polish.hotkey != to.polish.hotkey {
            set_value(table, "hotkey", to.polish.hotkey.as_str().into());
        }
        if from.polish.api_key != to.polish.api_key {
            set_value(table, "api_key", to.polish.api_key.as_str().into());
        }
    }
    Ok(doc.to_string())
}

/// Replaces a value in place, keeping the comments around it.
fn set_value(table: &mut Table, key: &str, mut value: Value) {
    if let Some(old) = table.get(key).and_then(Item::as_value) {
        *value.decor_mut() = old.decor().clone();
    }
    table[key] = Item::Value(value);
}

/// An inline array, or one item per line when that would get long.
fn string_array(items: &[String]) -> Value {
    let mut array: Array = items.iter().map(String::as_str).collect();
    if array.to_string().len() > 80 {
        for item in array.iter_mut() {
            item.decor_mut().set_prefix("\n    ");
            item.decor_mut().set_suffix("");
        }
        array.set_trailing("\n");
        array.set_trailing_comma(true);
    }
    Value::Array(array)
}

pub fn default_file() -> String {
    let hotkey = Config::default().hotkey;
    format!(
        r#"# Dictum settings. Most are also in tray menu → Settings…; after editing this file by hand,
# restart Dictum (tray menu → Restart).

# Hold this to dictate; release to insert the text.
# Examples: "ctrl+win", "right_ctrl", "right_alt", "ctrl+shift+space", "f13", "fn" (macOS).
hotkey = "{hotkey}"

# While holding the hotkey, tap this to keep recording hands-free.
# Press the hotkey again to finish.
hands_free_key = "space"

# Discards the current recording.
cancel_key = "escape"

# "paste": clipboard + Ctrl+V (fast, works almost everywhere).
# "type": synthetic key presses (never touches the clipboard; slower for long text).
insert_method = "paste"

# Put your previous clipboard contents back after pasting.
restore_clipboard = true

# Add a space after each dictation so consecutive dictations flow together.
trailing_space = true

# Drop hesitations like "um" and "uh".
remove_fillers = true

# Drop stuttered repeats: "I I I want to" -> "I want to", "w- want" -> "want".
remove_stutters = true

# Say "new line" or "new paragraph" to insert line breaks.
voice_commands = false

# Short sounds when recording starts and stops.
sounds = true

# A small bar at the bottom of the screen while Dictum listens (with live levels) and transcribes.
indicator = true

# Microphone to use: empty for the system default, or part of the device name.
microphone = ""

# "cpu" (default, fast everywhere) or "gpu" (DirectML on Windows, CoreML on macOS).
device = "cpu"

# Threads for speech recognition; 0 = automatic.
threads = 0

# Recordings stop automatically after this many seconds.
max_recording_secs = 600

# Words and names you use that speech recognition tends to get wrong, written the way you want
# them. Recognition favours them when the audio is ambiguous, and close misses are corrected
# ("versal" -> "Vercel", "turbo repo" -> "Turborepo"). Very short terms get a gentler nudge.
# Example: vocabulary = ["Claude Code", "Vercel", "shadcn", "TanStack", "Convex", "pnpm"]
vocabulary = []

# Personal dictionary: "what the model hears" = "what you want".
[replacements]
# "get hub" = "GitHub"

# A second hotkey whose dictations Claude rewrites into a clean coding prompt: self-corrections
# ("actually, scratch that") applied, sentences fixed, longer requests structured as a goal plus
# bullet points. The text is sent to Anthropic's API; audio never leaves this computer.
[polish]
# Empty turns it off. Examples: "right_option", "right_cmd", "fn+shift" (macOS), "right_ctrl".
hotkey = ""

# Anthropic API key (console.anthropic.com). Empty: the ANTHROPIC_API_KEY environment variable.
api_key = ""

model = "claude-haiku-5-5"

# If the polished text isn't back this long after you release the key, the plain text is
# inserted instead.
timeout_ms = 4000

# Extra instructions for the rewrite, e.g. "We use Next.js App Router, Tailwind and Convex."
instructions = ""
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_parses_to_defaults() {
        let parsed = Config::parse(&default_file()).unwrap();
        let defaults = Config::default();
        assert_eq!(parsed.hotkey, defaults.hotkey);
        assert_eq!(parsed.insert_method, InsertMethod::Paste);
        assert!(parsed.replacements.is_empty());
        assert!(parsed.vocabulary.is_empty());
        assert_eq!(parsed.max_recording_secs, 600);
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let c = Config::parse("hotkey = \"right_ctrl\"\n[replacements]\n\"get hub\" = \"GitHub\"\n").unwrap();
        assert_eq!(c.hotkey, "right_ctrl");
        assert!(c.sounds);
        assert_eq!(c.text_options().replacements, vec![("get hub".to_string(), "GitHub".to_string())]);
    }

    #[test]
    fn vocabulary_reaches_engine_and_text() {
        let c = Config::parse("vocabulary = [\"Claude Code\", \"Vercel\"]\n").unwrap();
        let expected = vec!["Claude Code".to_string(), "Vercel".to_string()];
        assert_eq!(c.engine_options().vocabulary, expected);
        assert_eq!(c.text_options().vocabulary, expected);
    }

    #[test]
    fn invalid_hotkey_is_rejected() {
        assert!(Config::parse("hotkey = \"ctrl+banana\"").is_err());
        assert!(Config::parse("insert_method = \"teleport\"").is_err());
    }

    #[test]
    fn polish_section_is_optional_and_validated() {
        let plain = Config::parse("hotkey = \"fn\"\n").unwrap();
        assert!(plain.bindings().unwrap().polish.is_none());
        assert_eq!(plain.polish.model, "claude-haiku-5-5");

        let c =
            Config::parse("hotkey = \"fn\"\n[polish]\nhotkey = \"right_option\"\napi_key = \" sk-test \"\n")
                .unwrap();
        assert_eq!(c.bindings().unwrap().polish, Some(Hotkey::parse("right_alt").unwrap()));
        assert_eq!(c.polish.api_key().as_deref(), Some("sk-test"));
        assert_eq!(c.polish.timeout_ms, 4000);

        assert!(Config::parse("hotkey = \"fn\"\n[polish]\nhotkey = \"fn\"\n").is_err());
        assert!(Config::parse("[polish]\nhotkey = \"right_alt+space\"\n").is_err());
        assert!(Config::parse("[polish]\nhotkey = \"banana\"\n").is_err());
    }

    #[test]
    fn edits_keep_comments_and_untouched_settings() {
        let text = "# My settings\n\n# Hold this.\nhotkey = \"control+super\" # mine\nthreads = 3\nsounds = true\n\n[replacements]\n# keep me\n\"get hub\" = \"GitHub\"\n\"versal\" = \"Vercel\"\n";
        let from = Config::parse(text).unwrap();
        let mut to = from.clone();
        to.hotkey = "right_ctrl".into();
        to.sounds = false;
        to.insert_method = InsertMethod::Type;
        to.vocabulary = vec!["Claude Code".into(), "pnpm".into()];
        to.replacements.remove("versal");
        to.replacements.insert("turbo repo".into(), "Turborepo".into());

        let edited = edit(text, &from, &to).unwrap();
        let expected = "# My settings\n\n# Hold this.\nhotkey = \"right_ctrl\" # mine\nthreads = 3\nsounds = false\ninsert_method = \"type\"\nvocabulary = [\"Claude Code\", \"pnpm\"]\n\n[replacements]\n# keep me\n\"get hub\" = \"GitHub\"\n\"turbo repo\" = \"Turborepo\"\n";
        assert_eq!(edited, expected);
        let reparsed = Config::parse(&edited).unwrap();
        assert_eq!(reparsed.threads, 3);
        assert_eq!(reparsed.vocabulary, to.vocabulary);
        assert_eq!(reparsed.replacements, to.replacements);

        // Nothing changed: the text comes back byte for byte.
        assert_eq!(edit(text, &from, &from).unwrap(), text);
    }

    #[test]
    fn edits_the_default_file() {
        let text = default_file();
        let from = Config::parse(&text).unwrap();
        let mut to = from.clone();
        to.remove_fillers = false;
        to.vocabulary =
            ["Claude Code", "Vercel", "shadcn", "TanStack", "Convex", "pnpm", "Turborepo", "Next.js"]
                .map(String::from)
                .to_vec();
        to.replacements.insert("get hub".into(), "GitHub".into());
        let edited = edit(&text, &from, &to).unwrap();
        assert!(edited.contains("# Drop hesitations like \"um\" and \"uh\".\nremove_fillers = false\n"));
        assert!(edited.contains("vocabulary = [\n    \"Claude Code\",\n    \"Vercel\","));
        assert!(edited.contains("    \"Next.js\",\n]\n"));
        assert_eq!(
            edited.lines().filter(|l| l.starts_with('#')).count(),
            text.lines().filter(|l| l.starts_with('#')).count()
        );
        let reparsed = Config::parse(&edited).unwrap();
        assert!(!reparsed.remove_fillers);
        assert_eq!(reparsed.vocabulary, to.vocabulary);
        assert_eq!(reparsed.replacements, to.replacements);
    }

    #[test]
    fn api_key_is_written_into_the_polish_table() {
        // The default file has the table: the key replaces the empty one in place.
        let text = default_file();
        let from = Config::parse(&text).unwrap();
        let mut to = from.clone();
        to.polish.api_key = "sk-ant-test".into();
        let edited = edit(&text, &from, &to).unwrap();
        assert!(edited.contains("the ANTHROPIC_API_KEY environment variable.\napi_key = \"sk-ant-test\"\n"));
        assert_eq!(edited.matches("api_key").count(), text.matches("api_key").count());
        assert_eq!(Config::parse(&edited).unwrap().polish.api_key, "sk-ant-test");

        // An older file without one gets a new table at the end, after `[replacements]` and the
        // example under it.
        let text = "hotkey = \"fn\"\n\n[replacements]\n# \"get hub\" = \"GitHub\"\n";
        let from = Config::parse(text).unwrap();
        let mut to = from.clone();
        to.polish.hotkey = "right_cmd".into();
        let edited = edit(text, &from, &to).unwrap();
        assert_eq!(edited, format!("{text}\n[polish]\nhotkey = \"right_cmd\"\n"));

        let text = "hotkey = \"fn\"\n\n[replacements]\n\"get hub\" = \"GitHub\"\n";
        let from = Config::parse(text).unwrap();
        let mut to = from.clone();
        to.polish.api_key = "sk-ant-test".into();
        let edited = edit(text, &from, &to).unwrap();
        assert!(edited.starts_with(text));
        let reparsed = Config::parse(&edited).unwrap();
        assert_eq!(reparsed.polish.api_key, "sk-ant-test");
        assert_eq!(reparsed.replacements, from.replacements);

        // The hotkey goes into the same table, next to the key.
        let mut again = to.clone();
        again.polish.hotkey = "right_option".into();
        let twice = edit(&edited, &to, &again).unwrap();
        assert_eq!(twice.matches("[polish]").count(), 1);
        assert_eq!(Config::parse(&twice).unwrap().polish.hotkey, "right_option");
    }

    #[test]
    fn saves_only_when_something_changed() {
        let dir = std::env::temp_dir().join(format!("dictum-config-save-{}", std::process::id()));
        let path = dir.join("config.toml");
        let _ = std::fs::remove_dir_all(&dir);
        let from = Config::load_or_create(&path).unwrap();
        assert!(!save_changes(&path, &from, &from).unwrap());
        let mut to = from.clone();
        to.microphone = "USB".into();
        assert!(save_changes(&path, &from, &to).unwrap());
        assert_eq!(Config::load_or_create(&path).unwrap().microphone, "USB");
        assert!(!dir.join("config.toml.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn creates_file_on_first_run() {
        let dir = std::env::temp_dir().join(format!("dictum-config-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        let _ = std::fs::remove_dir_all(&dir);
        let c = Config::load_or_create(&path).unwrap();
        assert_eq!(c.hotkey, Config::default().hotkey);
        assert!(path.exists());
        assert!(Config::load_or_create(&path).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
