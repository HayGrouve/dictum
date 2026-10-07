//! "Start at login" as a per-user LaunchAgent pointing at the running copy of Dictum.

use std::path::PathBuf;

use anyhow::{Context, Result};

const LABEL: &str = "com.github.haygrouve.dictum";

fn plist_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

/// What launchd should start: the app bundle when we run from one, else the binary.
fn program() -> Result<Vec<String>> {
    if let Some(bundle) = super::bundle_path() {
        return Ok(vec!["/usr/bin/open".into(), bundle.display().to_string()]);
    }
    let exe = std::env::current_exe().context("cannot locate the running executable")?;
    Ok(vec![exe.display().to_string()])
}

pub fn is_enabled() -> bool {
    let (Some(path), Ok(program)) = (plist_path(), program()) else { return false };
    let Some(target) = program.last() else { return false };
    // Only counts when it starts this copy of Dictum.
    std::fs::read_to_string(path).is_ok_and(|plist| plist.contains(&xml_escape(target)))
}

pub fn set(enable: bool) -> Result<()> {
    let path = plist_path().context("no home directory")?;
    if !enable {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("failed to remove {}", path.display()))
            }
            _ => Ok(()),
        };
    }
    let arguments: String =
        program()?.iter().map(|a| format!("        <string>{}</string>\n", xml_escape(a))).collect();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{arguments}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, plist).with_context(|| format!("failed to write {}", path.display()))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
