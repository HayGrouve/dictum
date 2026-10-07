//! User settings: a commented TOML file, created with defaults on first run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::hotkey::{self, Hotkey, Key};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InsertMethod {
    /// Put the text on the clipboard and press Ctrl+V / Cmd+V (fast, works almost everywhere).
    Paste,
    /// Type the text as synthetic key presses (never touches the clipboard).
    Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceSetting {
    Cpu,
    Gpu,
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
    pub voice_commands: bool,
    pub sounds: bool,
    pub microphone: String,
    pub device: DeviceSetting,
    pub threads: usize,
    pub model_dir: Option<PathBuf>,
    pub max_recording_secs: u32,
    pub replacements: BTreeMap<String, String>,
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
            voice_commands: false,
            sounds: true,
            microphone: String::new(),
            device: DeviceSetting::Cpu,
            threads: 0,
            model_dir: None,
            max_recording_secs: 600,
            replacements: BTreeMap::new(),
        }
    }
}

/// Validated, ready-to-use key bindings.
#[derive(Debug, Clone)]
pub struct Bindings {
    pub hotkey: Hotkey,
    pub hands_free: Key,
    pub cancel: Key,
}

impl Config {
    pub fn bindings(&self) -> Result<Bindings> {
        let hotkey = Hotkey::parse(&self.hotkey).context("invalid `hotkey`")?;
        let hands_free = hotkey::parse_key(&self.hands_free_key).context("invalid `hands_free_key`")?;
        let cancel = hotkey::parse_key(&self.cancel_key).context("invalid `cancel_key`")?;
        Ok(Bindings { hotkey, hands_free, cancel })
    }

    pub fn text_options(&self) -> dictum_engine::text::TextOptions {
        dictum_engine::text::TextOptions {
            remove_fillers: self.remove_fillers,
            voice_commands: self.voice_commands,
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

pub fn default_file() -> String {
    let hotkey = Config::default().hotkey;
    format!(
        r#"# Dictum settings. Restart Dictum (tray menu → Restart) after editing.

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

# Say "new line" or "new paragraph" to insert line breaks.
voice_commands = false

# Short sounds when recording starts and stops.
sounds = true

# Microphone to use: empty for the system default, or part of the device name.
microphone = ""

# "cpu" (default, fast everywhere) or "gpu" (DirectML on Windows, CoreML on macOS).
device = "cpu"

# Threads for speech recognition; 0 = automatic.
threads = 0

# Recordings stop automatically after this many seconds.
max_recording_secs = 600

# Personal dictionary: "what the model hears" = "what you want".
[replacements]
# "get hub" = "GitHub"
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
    fn invalid_hotkey_is_rejected() {
        assert!(Config::parse("hotkey = \"ctrl+banana\"").is_err());
        assert!(Config::parse("insert_method = \"teleport\"").is_err());
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
