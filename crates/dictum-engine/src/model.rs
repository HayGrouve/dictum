//! Model manifest and a resumable, checksum-verified downloader.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

pub struct ModelFile {
    pub name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

pub struct ModelSpec {
    /// Directory name under the models folder.
    pub id: &'static str,
    pub base_url: &'static str,
    pub files: &'static [ModelFile],
}

/// Parakeet TDT 0.6B v3 (25 European languages, punctuation + capitalisation), int8 weights.
/// Pinned to an exact repository revision so checksums never drift.
pub const PARAKEET_TDT_V3_INT8: ModelSpec = ModelSpec {
    id: "parakeet-tdt-0.6b-v3-int8",
    base_url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce",
    files: &[
        ModelFile {
            name: "vocab.txt",
            size: 93_939,
            sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
        },
        ModelFile {
            name: "nemo128.onnx",
            size: 139_764,
            sha256: "a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f",
        },
        ModelFile {
            name: "decoder_joint-model.int8.onnx",
            size: 18_202_004,
            sha256: "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70",
        },
        ModelFile {
            name: "encoder-model.int8.onnx",
            size: 652_183_999,
            sha256: "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09",
        },
    ],
};

impl ModelSpec {
    pub fn dir_in(&self, models_root: &Path) -> PathBuf {
        models_root.join(self.id)
    }

    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Cheap presence check (sizes only); content was verified when it was downloaded.
    pub fn is_installed(&self, dir: &Path) -> bool {
        self.files
            .iter()
            .all(|f| fs::metadata(dir.join(f.name)).is_ok_and(|m| m.is_file() && m.len() == f.size))
    }

    /// Downloads every missing file into `dir`. Interrupted downloads resume from their
    /// `.part` file. `progress(done, total)` is called with byte counts across all files.
    pub fn download(
        &self,
        dir: &Path,
        cancel: &AtomicBool,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<()> {
        fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
        let total = self.total_size();
        let mut done = 0;
        for file in self.files {
            let target = dir.join(file.name);
            if fs::metadata(&target).is_ok_and(|m| m.len() == file.size) {
                done += file.size;
                progress(done, total);
                continue;
            }
            let url = format!("{}/{}", self.base_url, file.name);
            download_file(&url, &target, file, cancel, |n| progress(done + n, total))
                .with_context(|| format!("failed to download {}", file.name))?;
            done += file.size;
        }
        Ok(())
    }
}

fn download_file(
    url: &str,
    target: &Path,
    file: &ModelFile,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<()> {
    let part = target.with_file_name(format!("{}.part", file.name));
    let mut hasher = Sha256::new();
    let mut have = 0u64;
    if let Ok(mut existing) = File::open(&part) {
        have = io::copy(&mut existing, &mut HashWriter(&mut hasher))?;
        if have > file.size {
            have = 0;
            hasher = Sha256::new();
        }
    }

    let mut request = agent().get(url);
    if have > 0 {
        request = request.header("Range", format!("bytes={have}-"));
    }
    let mut response = request.call()?;
    let resumed = response.status() == 206;
    if !resumed && have > 0 {
        // Server ignored the range request: start over.
        have = 0;
        hasher = Sha256::new();
    }
    let mut out = OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(&part)
        .with_context(|| format!("failed to open {}", part.display()))?;

    let mut reader = response.body_mut().as_reader();
    let mut buf = vec![0u8; 1 << 20];
    progress(have);
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("download cancelled");
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        have += n as u64;
        progress(have);
    }
    out.sync_all()?;
    drop(out);

    let digest = hex(&hasher.finalize());
    if have != file.size || digest != file.sha256 {
        let _ = fs::remove_file(&part);
        bail!(
            "verification failed (got {have} bytes, sha256 {digest}; expected {} bytes, sha256 {})",
            file.size,
            file.sha256
        );
    }
    fs::rename(&part, target).with_context(|| format!("failed to move {}", part.display()))?;
    Ok(())
}

/// Fetches a small resource (at most `limit` bytes) over HTTPS with the OS certificate store.
pub fn http_get(url: &str, accept: &str, limit: u64) -> Result<Vec<u8>> {
    let mut response = agent()
        .get(url)
        .header("Accept", accept)
        .header("User-Agent", concat!("Dictum/", env!("CARGO_PKG_VERSION")))
        .call()
        .with_context(|| format!("request to {url} failed"))?;
    Ok(response.body_mut().with_config().limit(limit).read_to_vec()?)
}

fn agent() -> ureq::Agent {
    #[cfg(any(windows, target_os = "macos"))]
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    #[cfg(not(any(windows, target_os = "macos")))]
    let tls = ureq::tls::TlsConfig::default();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_connect(Some(std::time::Duration::from_secs(20)))
        .build()
        .new_agent()
}

struct HashWriter<'a>(&'a mut Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_consistent() {
        let spec = &PARAKEET_TDT_V3_INT8;
        assert!(spec.total_size() > 600_000_000);
        for f in spec.files {
            assert_eq!(f.sha256.len(), 64, "{}", f.name);
            assert!(f.sha256.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn missing_dir_is_not_installed() {
        assert!(!PARAKEET_TDT_V3_INT8.is_installed(Path::new("/definitely/not/here")));
    }

    #[test]
    fn hex_encodes() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
    }
}
