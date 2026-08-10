//! Verified libkrunfw download and per-user cache.

use anyhow::{bail, Context};
use flate2::read::GzDecoder;
use fs2::FileExt;
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::time::Duration;

pub(crate) const VERSION: &str = "5.5.0";
#[cfg(target_os = "macos")]
const ABI_VERSION: &str = "5";
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
struct ReleaseAsset {
    url: &'static str,
    sha256: &'static str,
    archive_member: &'static str,
}

#[derive(Debug, Clone)]
pub struct FirmwareStore {
    root: PathBuf,
    client: Client,
}

impl FirmwareStore {
    pub fn new() -> anyhow::Result<Self> {
        let root = dirs::cache_dir()
            .context("platform cache directory is unavailable")?
            .join("persisting/pvisor/firmware");
        fs::create_dir_all(&root)?;
        let client = Client::builder()
            .user_agent(concat!("pvisor/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(300))
            .build()
            .context("build libkrunfw download client")?;
        Ok(Self { root, client })
    }

    pub fn prepare(&self) -> anyhow::Result<PathBuf> {
        let platform = platform_name()?;
        let directory = self.root.join(VERSION).join(platform);
        let firmware = directory.join(crate::vm::firmware_name());
        if firmware.is_file() {
            return Ok(directory);
        }

        fs::create_dir_all(&directory)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join(format!("{VERSION}-{platform}.lock")))?;
        lock.lock_exclusive()?;
        if firmware.is_file() {
            FileExt::unlock(&lock)?;
            return Ok(directory);
        }

        let result = self.install(&directory, &firmware);
        FileExt::unlock(&lock)?;
        result?;
        Ok(directory)
    }

    fn install(&self, directory: &Path, firmware: &Path) -> anyhow::Result<()> {
        let asset = release_asset()?;
        let archive = self.load_archive(directory, asset)?;

        let temporary = tempfile::Builder::new()
            .prefix(".libkrunfw-")
            .tempdir_in(directory)?;
        let payload = temporary.path().join("kernel.c");
        extract_member(&archive, asset.archive_member, &payload)?;
        let built = temporary.path().join(crate::vm::firmware_name());
        build_platform_firmware(&payload, &built)?;
        let mut permissions = fs::metadata(&built)?.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o755);
        }
        fs::set_permissions(&built, permissions)?;
        fs::rename(&built, firmware)
            .with_context(|| format!("install cached firmware at {}", firmware.display()))?;
        fs::write(
            directory.join("source.sha256"),
            format!("{}  {}\n", asset.sha256, asset.url),
        )?;
        Ok(())
    }

    fn load_archive(&self, directory: &Path, asset: ReleaseAsset) -> anyhow::Result<Vec<u8>> {
        let cached = directory.join("source.tgz");
        if cached.is_file() {
            let archive = fs::read(&cached)?;
            verify_archive(&archive, asset.sha256)?;
            return Ok(archive);
        }
        let mut response = self
            .client
            .get(asset.url)
            .send()
            .with_context(|| format!("download libkrunfw {VERSION}"))?
            .error_for_status()
            .context("libkrunfw release download failed")?;
        if let Some(length) = response.content_length() {
            anyhow::ensure!(
                length <= MAX_ARCHIVE_BYTES as u64,
                "libkrunfw archive is unexpectedly large: {length} bytes"
            );
        }
        let mut archive = Vec::new();
        response
            .by_ref()
            .take(MAX_ARCHIVE_BYTES as u64 + 1)
            .read_to_end(&mut archive)?;
        verify_archive(&archive, asset.sha256)?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(&archive)?;
        temporary.flush()?;
        match temporary.persist_noclobber(&cached) {
            Ok(_) => {}
            Err(error) if cached.is_file() => drop(error),
            Err(error) => return Err(error.error.into()),
        }
        Ok(archive)
    }
}

fn verify_archive(archive: &[u8], expected: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        archive.len() <= MAX_ARCHIVE_BYTES,
        "libkrunfw archive exceeds {} bytes",
        MAX_ARCHIVE_BYTES
    );
    let actual = encode_hex(&Sha256::digest(archive));
    anyhow::ensure!(
        actual == expected,
        "libkrunfw archive digest mismatch: expected {expected}, got {actual}"
    );
    Ok(())
}

fn platform_name() -> anyhow::Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("macos-aarch64"),
        ("linux", "aarch64") => Ok("linux-aarch64"),
        ("linux", "x86_64") => Ok("linux-x86_64"),
        (os, arch) => bail!("automatic libkrunfw installation is unsupported on {os}/{arch}"),
    }
}

fn release_asset() -> anyhow::Result<ReleaseAsset> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok(ReleaseAsset {
            url: "https://github.com/libkrun/libkrunfw/releases/download/v5.5.0/libkrunfw-prebuilt-aarch64.tgz",
            sha256: "5bfae6efee63dbdf04a8fac2a69d772d9f900af2f54c4429b4acdfd6d86b9979",
            archive_member: "libkrunfw/kernel.c",
        }),
        ("linux", "aarch64") => Ok(ReleaseAsset {
            url: "https://github.com/libkrun/libkrunfw/releases/download/v5.5.0/libkrunfw-aarch64.tgz",
            sha256: "b04c9a5520a1ea52b5b35d87559566872246145961c4b6978034c9b9be54b89b",
            archive_member: "lib64/libkrunfw.so.5.5.0",
        }),
        ("linux", "x86_64") => Ok(ReleaseAsset {
            url: "https://github.com/libkrun/libkrunfw/releases/download/v5.5.0/libkrunfw-x86_64.tgz",
            sha256: "c169206b01c89fbe134f1728bf4f988702bc7f73b4cf73e6fdece447d6fceca1",
            archive_member: "lib64/libkrunfw.so.5.5.0",
        }),
        (os, arch) => bail!("automatic libkrunfw installation is unsupported on {os}/{arch}"),
    }
}

fn extract_member(archive: &[u8], expected: &str, destination: &Path) -> anyhow::Result<()> {
    let decoder = GzDecoder::new(archive);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.as_ref() != Path::new(expected) {
            continue;
        }
        anyhow::ensure!(
            entry.header().entry_type().is_file(),
            "libkrunfw archive member {expected} is not a regular file"
        );
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(destination)?;
        std::io::copy(&mut entry, &mut output)?;
        output.flush()?;
        return Ok(());
    }
    bail!("libkrunfw archive is missing {expected}")
}

#[cfg(target_os = "macos")]
fn build_platform_firmware(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let output = Command::new("/usr/bin/cc")
        .arg("-fPIC")
        .arg(format!("-DABI_VERSION={ABI_VERSION}"))
        .arg("-shared")
        .arg("-Wl,-install_name,@rpath/libkrunfw.5.dylib")
        .arg("-o")
        .arg(destination)
        .arg(source)
        .output()
        .context("compile downloaded libkrunfw payload with /usr/bin/cc")?;
    anyhow::ensure!(
        output.status.success(),
        "compile downloaded libkrunfw payload: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn build_platform_firmware(source: &Path, destination: &Path) -> anyhow::Result<()> {
    fs::copy(source, destination)?;
    Ok(())
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_digest_is_pinned_sha256() {
        let asset = release_asset().unwrap();
        assert_eq!(asset.sha256.len(), 64);
        assert!(asset.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(asset.url.contains(&format!("/v{VERSION}/")));
    }
}
