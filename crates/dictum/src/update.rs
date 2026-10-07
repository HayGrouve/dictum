//! Updates from GitHub Releases, only when the user asks (tray → Check for updates).
//!
//! The release zip is downloaded into memory and checked against the SHA-256 digest GitHub
//! publishes for the asset. Nothing on disk changes until every new file has been written next
//! to the old ones. Files are then swapped by renaming, which works even for the running exe and
//! its loaded DLLs on Windows. If a rename fails, everything is put back. The renamed old files
//! are deleted on the next start.

use std::fmt;
use std::io::{Cursor, Read};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const LATEST_RELEASE: &str = "https://api.github.com/repos/HayGrouve/dictum/releases/latest";
const ASSET: &str = "dictum-windows-x64.zip";
const EXE: &str = "dictum.exe";
/// Suffixes of files left behind by an update; only these are ever cleaned up.
const NEW_SUFFIX: &str = ".dictum-new";
const OLD_SUFFIX: &str = ".dictum-old";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    /// "0.2.0" or "v0.2.0"; pre-release and build suffixes are not supported.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().strip_prefix('v').unwrap_or(text.trim()).split('.');
        let mut next = || parts.next()?.parse().ok();
        let version = Self(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }

    /// The running build's version. `DICTUM_PRETEND_VERSION` overrides it to try updating.
    pub fn current() -> Self {
        std::env::var("DICTUM_PRETEND_VERSION")
            .ok()
            .and_then(|v| Self::parse(&v))
            .or_else(|| Self::parse(env!("CARGO_PKG_VERSION")))
            .expect("CARGO_PKG_VERSION is a plain version")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Debug, Clone)]
pub struct Release {
    pub version: Version,
    pub notes: String,
    url: String,
    sha256: String,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

/// Parses GitHub's "latest release" response.
fn parse_release(json: &[u8]) -> Result<Release> {
    let api: ApiRelease = serde_json::from_slice(json).context("unexpected response from GitHub")?;
    let version = Version::parse(&api.tag_name)
        .with_context(|| format!("release tag {:?} is not a version", api.tag_name))?;
    let asset = api
        .assets
        .into_iter()
        .find(|a| a.name == ASSET)
        .with_context(|| format!("release {} has no {ASSET}", api.tag_name))?;
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .with_context(|| format!("GitHub published no checksum for {ASSET}"))?
        .to_ascii_lowercase();
    Ok(Release {
        version,
        notes: api.body.unwrap_or_default().trim().to_string(),
        url: asset.browser_download_url,
        sha256,
    })
}

/// Asks GitHub for the latest release; `Some` if it is newer than `current`.
pub fn check(current: Version) -> Result<Option<Release>> {
    let json = dictum_engine::model::http_get(LATEST_RELEASE, "application/vnd.github+json", 1 << 20)?;
    let release = parse_release(&json)?;
    Ok((release.version > current).then_some(release))
}

/// Downloads `release`, verifies it and replaces the files in `dir`.
pub fn install(release: &Release, dir: &Path) -> Result<()> {
    let zip = dictum_engine::model::http_get(&release.url, "application/octet-stream", 500 << 20)
        .context("download failed")?;
    let digest: String = Sha256::digest(&zip).iter().map(|b| format!("{b:02x}")).collect();
    ensure!(digest == release.sha256, "download is corrupt (checksum mismatch)");
    let files = unpack(&zip)?;
    replace_files(dir, &files)
}

/// Reads the flat file list of a release zip.
fn unpack(zip: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip)).context("download is not a zip file")?;
    let mut files = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        // Release zips are flat; Windows tools may still write backslashes.
        let name = entry.name().rsplit(['/', '\\']).next().unwrap_or_default().to_string();
        if name.is_empty() || name.starts_with('.') {
            bail!("unexpected file {:?} in the update", entry.name());
        }
        let mut data = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut data)?;
        files.push((name, data));
    }
    ensure!(files.iter().any(|(name, _)| name == EXE), "the update does not contain {EXE}");
    Ok(files)
}

/// Writes every file as `<name>.dictum-new` first, then swaps them in. The current files are
/// renamed to `<name>.dictum-old`; on failure the swap is undone.
fn replace_files(dir: &Path, files: &[(String, Vec<u8>)]) -> Result<()> {
    remove_leftovers(dir);
    for (name, data) in files {
        let staged = dir.join(format!("{name}{NEW_SUFFIX}"));
        if let Err(e) = std::fs::write(&staged, data) {
            remove_leftovers(dir);
            return Err(e).with_context(|| format!("cannot write {}", staged.display()));
        }
    }
    let mut done: Vec<(&str, bool)> = Vec::new();
    for (name, _) in files {
        match swap_in(dir, name) {
            Ok(had_old) => done.push((name, had_old)),
            Err(e) => {
                for &(name, had_old) in done.iter().rev() {
                    let _ = std::fs::remove_file(dir.join(name));
                    if had_old {
                        let _ = std::fs::rename(dir.join(format!("{name}{OLD_SUFFIX}")), dir.join(name));
                    }
                }
                remove_leftovers(dir);
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Moves `name` aside (if present) and `name.dictum-new` into its place; returns whether there
/// was an old file.
fn swap_in(dir: &Path, name: &str) -> Result<bool> {
    let target = dir.join(name);
    let old = dir.join(format!("{name}{OLD_SUFFIX}"));
    let had_old = target.exists();
    if had_old {
        std::fs::rename(&target, &old).with_context(|| format!("cannot move {} aside", target.display()))?;
    }
    if let Err(e) = std::fs::rename(dir.join(format!("{name}{NEW_SUFFIX}")), &target) {
        if had_old {
            let _ = std::fs::rename(&old, &target);
        }
        return Err(e).with_context(|| format!("cannot replace {}", target.display()));
    }
    Ok(had_old)
}

/// Deletes files left by an earlier update; returns how many are still there. Old files may be
/// in use for a moment while the previous process exits.
pub fn remove_leftovers(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.ends_with(NEW_SUFFIX) || name.ends_with(OLD_SUFFIX)
        })
        .filter(|e| std::fs::remove_file(e.path()).is_err())
        .count()
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(Version::parse("v0.2.0"), Some(Version(0, 2, 0)));
        assert_eq!(Version::parse("1.10.3"), Some(Version(1, 10, 3)));
        assert!(Version::parse("v1.2").is_none());
        assert!(Version::parse("1.2.3.4").is_none());
        assert!(Version::parse("1.2.3-beta").is_none());
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert!(Version(1, 0, 0) > Version(0, 99, 0));
        assert_eq!(Version(0, 2, 0).to_string(), "0.2.0");
        assert!(Version::parse(env!("CARGO_PKG_VERSION")).is_some());
    }

    fn release_json(digest: &str) -> String {
        format!(
            r#"{{"tag_name": "v0.3.0", "body": "What's new:\n- faster\n", "assets": [
                {{"name": "other.zip", "browser_download_url": "https://x/other.zip", "digest": null}},
                {{"name": "dictum-windows-x64.zip", "browser_download_url": "https://x/d.zip", "digest": "{digest}"}}
            ]}}"#
        )
    }

    #[test]
    fn parses_github_release() {
        let r = parse_release(release_json("sha256:ABCDEF").as_bytes()).unwrap();
        assert_eq!(r.version, Version(0, 3, 0));
        assert_eq!(r.url, "https://x/d.zip");
        assert_eq!(r.sha256, "abcdef");
        assert_eq!(r.notes, "What's new:\n- faster");
    }

    #[test]
    fn rejects_release_without_checksum_or_asset() {
        assert!(parse_release(release_json("md5:x").as_bytes()).is_err());
        assert!(parse_release(br#"{"tag_name": "v0.3.0", "assets": []}"#).is_err());
        assert!(parse_release(br#"{"tag_name": "latest", "assets": []}"#).is_err());
    }

    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in files {
            writer.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(data.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn unpacks_flat_release_zip() {
        let files = unpack(&zip_of(&[("dictum.exe", "exe"), ("dist\\a.dll", "dll")])).unwrap();
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["dictum.exe", "a.dll"]);
        assert!(unpack(&zip_of(&[("a.dll", "dll")])).is_err(), "must contain the exe");
        assert!(unpack(b"not a zip").is_err());
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("dictum-update-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn files(&self) -> Vec<(String, String)> {
            let mut files: Vec<(String, String)> = std::fs::read_dir(&self.0)
                .unwrap()
                .flatten()
                .map(|e| {
                    let content = std::fs::read_to_string(e.path()).unwrap_or_default();
                    (e.file_name().to_string_lossy().into_owned(), content)
                })
                .collect();
            files.sort();
            files
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn owned(files: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
        files.iter().map(|(n, d)| (n.to_string(), d.as_bytes().to_vec())).collect()
    }

    fn strings(files: &[(&str, &str)]) -> Vec<(String, String)> {
        files.iter().map(|(n, d)| (n.to_string(), d.to_string())).collect()
    }

    #[test]
    fn replaces_files_and_keeps_others() {
        let dir = TempDir::new("replace");
        std::fs::write(dir.0.join("dictum.exe"), "v1").unwrap();
        std::fs::write(dir.0.join("a.dll"), "a1").unwrap();
        std::fs::write(dir.0.join("notes.txt"), "mine").unwrap();
        replace_files(&dir.0, &owned(&[("dictum.exe", "v2"), ("a.dll", "a2"), ("b.dll", "b2")])).unwrap();
        assert_eq!(
            dir.files(),
            strings(&[
                ("a.dll", "a2"),
                ("a.dll.dictum-old", "a1"),
                ("b.dll", "b2"),
                ("dictum.exe", "v2"),
                ("dictum.exe.dictum-old", "v1"),
                ("notes.txt", "mine"),
            ])
        );
        assert_eq!(remove_leftovers(&dir.0), 0);
        assert_eq!(
            dir.files(),
            strings(&[("a.dll", "a2"), ("b.dll", "b2"), ("dictum.exe", "v2"), ("notes.txt", "mine")])
        );
    }

    /// What updating relies on: Windows lets a running exe be renamed and replaced.
    #[cfg(windows)]
    #[test]
    fn replaces_a_running_exe() {
        let dir = TempDir::new("running");
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let exe = dir.0.join("dictum.exe");
        std::fs::copy(Path::new(&system).join(r"System32\PING.EXE"), &exe).unwrap();
        let mut child = std::process::Command::new(&exe)
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let result = replace_files(&dir.0, &owned(&[("dictum.exe", "v2")]));
        child.kill().unwrap();
        child.wait().unwrap();
        result.unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "v2");
        assert_eq!(remove_leftovers(&dir.0), 0);
    }

    #[test]
    fn failed_swap_restores_the_old_files() {
        let dir = TempDir::new("rollback");
        std::fs::write(dir.0.join("dictum.exe"), "v1").unwrap();
        std::fs::write(dir.0.join("a.dll"), "a1").unwrap();
        std::fs::write(dir.0.join("z.dll"), "z1").unwrap();
        // A directory in the way of moving the last file aside makes its swap fail.
        let blocker = dir.0.join("z.dll.dictum-old");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("keep"), "").unwrap();
        let result = replace_files(&dir.0, &owned(&[("dictum.exe", "v2"), ("a.dll", "a2"), ("z.dll", "z2")]));
        assert!(result.is_err());
        let files: Vec<(String, String)> =
            dir.files().into_iter().filter(|(n, _)| n != "z.dll.dictum-old").collect();
        assert_eq!(files, strings(&[("a.dll", "a1"), ("dictum.exe", "v1"), ("z.dll", "z1")]));
    }
}
