//! Minimal OCI Distribution client and content-addressed rootfs store.
//!
//! This deliberately owns the small public-image path instead of shelling out
//! to Docker, Podman, or Buildah. The on-disk layout is private to pVisor; OCI
//! digests remain the source of truth for blobs and prepared root filesystems.

use anyhow::{bail, Context};
use flate2::read::GzDecoder;
use fs2::FileExt;
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, AUTHORIZATION, WWW_AUTHENTICATE};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

pub const DEFAULT_IMAGE: &str = "ubuntu:latest";

const MANIFEST_ACCEPT: &str = concat!(
    "application/vnd.oci.image.index.v1+json, ",
    "application/vnd.docker.distribution.manifest.list.v2+json, ",
    "application/vnd.oci.image.manifest.v1+json, ",
    "application/vnd.docker.distribution.manifest.v2+json"
);

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImageReference {
    registry: String,
    repository: String,
    reference: String,
}

#[derive(Debug, Clone)]
pub struct PreparedImage {
    pub rootfs: PathBuf,
    pub digest: String,
    pub env: BTreeMap<String, String>,
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ImageStore {
    root: PathBuf,
    client: Client,
}

#[derive(Debug, Clone, Deserialize)]
struct Descriptor {
    #[serde(rename = "mediaType", default)]
    media_type: String,
    digest: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    platform: Option<Platform>,
}

#[derive(Debug, Clone, Deserialize)]
struct Platform {
    architecture: String,
    os: String,
    #[serde(default)]
    variant: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImageIndex {
    manifests: Vec<Descriptor>,
}

#[derive(Debug, Deserialize)]
struct ImageManifest {
    config: Descriptor,
    layers: Vec<Descriptor>,
}

#[derive(Debug, Default, Deserialize)]
struct ImageConfiguration {
    #[serde(default)]
    config: RuntimeConfiguration,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RuntimeConfiguration {
    #[serde(default)]
    env: Option<Vec<String>>,
    #[serde(default)]
    entrypoint: Option<Vec<String>>,
    #[serde(default)]
    cmd: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    token: String,
    #[serde(default)]
    access_token: String,
}

#[derive(Debug, Serialize)]
struct RootfsMetadata<'a> {
    image: &'a str,
    manifest_digest: &'a str,
}

impl ImageStore {
    pub fn new(root: Option<PathBuf>) -> anyhow::Result<Self> {
        let root = match root {
            Some(root) => root,
            None => default_store_dir()?,
        };
        fs::create_dir_all(root.join("blobs/sha256"))?;
        fs::create_dir_all(root.join("rootfs-v3/sha256"))?;
        fs::create_dir_all(root.join("metadata/sha256"))?;
        fs::create_dir_all(root.join("locks"))?;
        let client = Client::builder()
            .user_agent(concat!("pvisor/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("build OCI registry client")?;
        Ok(Self { root, client })
    }

    pub fn prepare(&self, image: &str) -> anyhow::Result<PreparedImage> {
        let image_ref = ImageReference::parse(image)?;
        let mut registry = RegistryClient::new(&self.client, image_ref.clone());
        let (mut body, mut manifest_digest) = registry.fetch_manifest(&image_ref.reference)?;
        let value: serde_json::Value = serde_json::from_slice(&body)
            .with_context(|| format!("decode OCI manifest for {image}"))?;
        if value.get("manifests").is_some() {
            let index: ImageIndex = serde_json::from_value(value)?;
            let descriptor = select_platform(&index.manifests)?;
            let fetched = registry.fetch_manifest(&descriptor.digest)?;
            body = fetched.0;
            manifest_digest = fetched.1;
            verify_digest(&descriptor.digest, &body)?;
        }
        let manifest: ImageManifest = serde_json::from_slice(&body)
            .with_context(|| format!("decode image manifest for {image}"))?;
        let config_path = self.fetch_blob(&mut registry, &manifest.config)?;
        let config: ImageConfiguration = serde_json::from_reader(File::open(config_path)?)
            .with_context(|| format!("decode image configuration for {image}"))?;

        let digest_hex = digest_hex(&manifest_digest)?;
        let rootfs = self.root.join("rootfs-v3/sha256").join(digest_hex);
        let lock_path = self.root.join("locks").join(format!("{digest_hex}.lock"));
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        lock.lock_exclusive()?;
        if !rootfs.is_dir() {
            let partial = self
                .root
                .join("rootfs-v3/sha256")
                .join(format!(".{digest_hex}.partial-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&partial)?;
            let extraction = (|| -> anyhow::Result<()> {
                for layer in &manifest.layers {
                    let blob = self.fetch_blob(&mut registry, layer)?;
                    apply_layer(&blob, &layer.media_type, &partial)?;
                }
                fs::rename(&partial, &rootfs)?;
                Ok(())
            })();
            if extraction.is_err() {
                let _ = fs::remove_dir_all(&partial);
            }
            extraction?;
            let metadata = RootfsMetadata {
                image,
                manifest_digest: &manifest_digest,
            };
            fs::write(
                self.root
                    .join("metadata/sha256")
                    .join(format!("{digest_hex}.json")),
                serde_json::to_vec_pretty(&metadata)?,
            )?;
        }
        lock.unlock()?;

        Ok(PreparedImage {
            rootfs,
            digest: manifest_digest,
            env: parse_env(config.config.env.unwrap_or_default()),
            entrypoint: config.config.entrypoint.unwrap_or_default(),
            cmd: config.config.cmd.unwrap_or_default(),
        })
    }

    fn fetch_blob(
        &self,
        registry: &mut RegistryClient<'_>,
        descriptor: &Descriptor,
    ) -> anyhow::Result<PathBuf> {
        let digest_hex = digest_hex(&descriptor.digest)?;
        let destination = self.root.join("blobs/sha256").join(digest_hex);
        if destination.is_file() {
            verify_file_digest(&descriptor.digest, &destination)?;
            return Ok(destination);
        }
        let mut response = registry.get(
            &format!(
                "/v2/{}/blobs/{}",
                registry.image.repository, descriptor.digest
            ),
            None,
        )?;
        let mut temporary = tempfile::NamedTempFile::new_in(self.root.join("blobs/sha256"))?;
        let mut hasher = Sha256::new();
        let mut size = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = response.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            temporary.write_all(&buffer[..read])?;
            size += read as u64;
        }
        let actual = format!("sha256:{}", encode_hex(&hasher.finalize()));
        anyhow::ensure!(
            actual == descriptor.digest,
            "OCI blob digest mismatch: expected {}, got {actual}",
            descriptor.digest
        );
        if let Some(expected) = descriptor.size {
            anyhow::ensure!(
                expected == size,
                "OCI blob size mismatch for {}: expected {expected}, got {size}",
                descriptor.digest
            );
        }
        match temporary.persist_noclobber(&destination) {
            Ok(_) => {}
            Err(error) if destination.is_file() => drop(error),
            Err(error) => return Err(error.error.into()),
        }
        Ok(destination)
    }
}

struct RegistryClient<'a> {
    client: &'a Client,
    image: ImageReference,
    token: Option<String>,
}

impl<'a> RegistryClient<'a> {
    fn new(client: &'a Client, image: ImageReference) -> Self {
        Self {
            client,
            image,
            token: None,
        }
    }

    fn fetch_manifest(&mut self, reference: &str) -> anyhow::Result<(Vec<u8>, String)> {
        let path = format!("/v2/{}/manifests/{reference}", self.image.repository);
        let mut response = self.get(&path, Some(MANIFEST_ACCEPT))?;
        let mut body = Vec::new();
        response.read_to_end(&mut body)?;
        let digest = format!("sha256:{}", encode_hex(&Sha256::digest(&body)));
        if reference.starts_with("sha256:") {
            verify_digest(reference, &body)?;
        }
        Ok((body, digest))
    }

    fn get(&mut self, path: &str, accept: Option<&str>) -> anyhow::Result<Response> {
        let url = format!("https://{}{}", self.image.registry, path);
        let send = |token: Option<&str>| {
            let mut request = self.client.get(&url);
            if let Some(accept) = accept {
                request = request.header(ACCEPT, accept);
            }
            if let Some(token) = token {
                request = request.header(AUTHORIZATION, format!("Bearer {token}"));
            }
            request.send()
        };
        let mut response = send(self.token.as_deref())?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            let challenge = response
                .headers()
                .get(WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok())
                .context("OCI registry omitted the Bearer authentication challenge")?;
            let token = self.fetch_token(challenge)?;
            self.token = Some(token);
            response = send(self.token.as_deref())?;
        }
        response
            .error_for_status()
            .with_context(|| format!("request OCI registry URL {url}"))
    }

    fn fetch_token(&self, challenge: &str) -> anyhow::Result<String> {
        let fields = parse_bearer_challenge(challenge)?;
        let realm = fields
            .get("realm")
            .context("Bearer challenge has no realm")?;
        let mut request = self.client.get(realm);
        if let Some(service) = fields.get("service") {
            request = request.query(&[("service", service)]);
        }
        let scope = fields
            .get("scope")
            .cloned()
            .unwrap_or_else(|| format!("repository:{}:pull", self.image.repository));
        request = request.query(&[("scope", &scope)]);
        let response: TokenResponse = request.send()?.error_for_status()?.json()?;
        let token = if response.token.is_empty() {
            response.access_token
        } else {
            response.token
        };
        anyhow::ensure!(
            !token.is_empty(),
            "registry token response contained no token"
        );
        Ok(token)
    }
}

impl ImageReference {
    fn parse(value: &str) -> anyhow::Result<Self> {
        let value = value
            .strip_prefix("docker://")
            .or_else(|| value.strip_prefix("oci://"))
            .unwrap_or(value);
        anyhow::ensure!(!value.trim().is_empty(), "OCI image reference is empty");
        let (name, reference) = if let Some((name, digest)) = value.rsplit_once('@') {
            (name, digest.to_owned())
        } else {
            let last_slash = value.rfind('/');
            let last_colon = value.rfind(':');
            if last_colon.is_some_and(|colon| last_slash.is_none_or(|slash| colon > slash)) {
                let colon = last_colon.expect("checked above");
                (&value[..colon], value[colon + 1..].to_owned())
            } else {
                (value, "latest".to_owned())
            }
        };
        anyhow::ensure!(
            !name.is_empty() && !reference.is_empty(),
            "invalid OCI image reference `{value}`"
        );
        let mut parts = name.split('/');
        let first = parts.next().expect("non-empty above");
        let explicit_registry = first.contains('.') || first.contains(':') || first == "localhost";
        let (registry, mut repository) = if explicit_registry {
            (first.to_owned(), parts.collect::<Vec<_>>().join("/"))
        } else {
            ("registry-1.docker.io".to_owned(), name.to_owned())
        };
        anyhow::ensure!(
            !repository.is_empty(),
            "image reference `{value}` has no repository"
        );
        let registry = match registry.as_str() {
            "docker.io" | "index.docker.io" => "registry-1.docker.io".to_owned(),
            _ => registry,
        };
        if registry == "registry-1.docker.io" && !repository.contains('/') {
            repository = format!("library/{repository}");
        }
        Ok(Self {
            registry,
            repository,
            reference,
        })
    }
}

fn default_store_dir() -> anyhow::Result<PathBuf> {
    if let Some(value) = std::env::var_os("PERSISTING_PVISOR_IMAGE_STORE") {
        return Ok(PathBuf::from(value));
    }
    Ok(dirs::cache_dir()
        .unwrap_or(std::env::current_dir()?.join(".persisting/cache"))
        .join("persisting/pvisor/images"))
}

fn select_platform(manifests: &[Descriptor]) -> anyhow::Result<&Descriptor> {
    let architecture = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => bail!("libkrun OCI images are unsupported on host architecture {other}"),
    };
    manifests
        .iter()
        .find(|descriptor| {
            descriptor.platform.as_ref().is_some_and(|platform| {
                platform.os == "linux"
                    && platform.architecture == architecture
                    && (architecture != "arm64"
                        || platform
                            .variant
                            .as_deref()
                            .is_none_or(|value| value == "v8"))
            })
        })
        .with_context(|| format!("image has no linux/{architecture} manifest"))
}

fn parse_env(values: Vec<String>) -> BTreeMap<String, String> {
    values
        .into_iter()
        .filter_map(|value| {
            let (key, value) = value.split_once('=')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn parse_bearer_challenge(value: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let value = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .context("OCI registry requested unsupported authentication")?;
    let mut result = BTreeMap::new();
    for field in value.split(',') {
        let (key, value) = field
            .trim()
            .split_once('=')
            .context("invalid Bearer challenge")?;
        result.insert(key.to_owned(), value.trim_matches('"').to_owned());
    }
    Ok(result)
}

fn digest_hex(digest: &str) -> anyhow::Result<&str> {
    let value = digest
        .strip_prefix("sha256:")
        .context("pVisor v1 only supports sha256 OCI digests")?;
    anyhow::ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid sha256 digest `{digest}`"
    );
    Ok(value)
}

fn verify_digest(expected: &str, body: &[u8]) -> anyhow::Result<()> {
    digest_hex(expected)?;
    let actual = format!("sha256:{}", encode_hex(&Sha256::digest(body)));
    anyhow::ensure!(
        actual == expected,
        "digest mismatch: expected {expected}, got {actual}"
    );
    Ok(())
}

fn verify_file_digest(expected: &str, path: &Path) -> anyhow::Result<()> {
    digest_hex(expected)?;
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("sha256:{}", encode_hex(&hasher.finalize()));
    anyhow::ensure!(
        actual == expected,
        "cached OCI blob {} is corrupt",
        path.display()
    );
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

fn apply_layer(blob: &Path, media_type: &str, rootfs: &Path) -> anyhow::Result<()> {
    let tar_file = tempfile::NamedTempFile::new()?;
    {
        let input = File::open(blob)?;
        let mut output = tar_file.reopen()?;
        if media_type.ends_with("+gzip") {
            std::io::copy(&mut GzDecoder::new(input), &mut output)?;
        } else if media_type.ends_with("+zstd") {
            std::io::copy(&mut zstd::stream::read::Decoder::new(input)?, &mut output)?;
        } else if media_type.ends_with(".tar")
            || media_type == "application/octet-stream"
            || media_type.is_empty()
        {
            std::io::copy(&mut std::io::BufReader::new(input), &mut output)?;
        } else {
            bail!("unsupported OCI layer media type `{media_type}`");
        }
    }

    let mut archive = tar::Archive::new(tar_file.reopen()?);
    for entry in archive.entries()? {
        let entry = entry?;
        let relative = clean_relative(&entry.path()?)?;
        if let Some(whiteout) = whiteout(&relative) {
            match whiteout {
                Whiteout::Remove(path) => remove_relative(rootfs, &path)?,
                Whiteout::Opaque(path) => clear_relative_directory(rootfs, &path)?,
            }
        }
    }

    let mut archive = tar::Archive::new(tar_file.reopen()?);
    archive.set_preserve_permissions(true);
    #[cfg(unix)]
    let mut deferred_directory_modes = BTreeMap::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let relative = clean_relative(&entry.path()?)?;
        if whiteout(&relative).is_some() {
            continue;
        }
        #[cfg(unix)]
        make_ancestor_directories_writable(rootfs, &relative, &mut deferred_directory_modes)?;
        // A rootless extractor cannot preserve an OCI entry's uid/gid. On
        // Linux, retaining setuid/setgid would therefore grant the host
        // user's identity inside the guest instead of the image owner. pVisor
        // VM workloads currently start as guest root, so stripping the bits
        // preserves execution while avoiding that incorrect privilege shift.
        #[cfg(target_os = "linux")]
        let sanitized_mode = entry.header().mode()? & !0o6000;
        #[cfg(unix)]
        let directory_mode = if entry.header().entry_type().is_dir() {
            Some({
                #[cfg(target_os = "linux")]
                {
                    sanitized_mode
                }
                #[cfg(not(target_os = "linux"))]
                {
                    entry.header().mode()?
                }
            })
        } else {
            None
        };
        #[cfg(target_os = "macos")]
        let linux_owner = (
            entry.header().uid().unwrap_or(0),
            entry.header().gid().unwrap_or(0),
        );
        let unpacked = entry.unpack_in(rootfs)?;
        anyhow::ensure!(
            unpacked,
            "OCI layer entry escaped rootfs: {}",
            relative.display()
        );
        #[cfg(target_os = "linux")]
        if !entry.header().entry_type().is_dir() && !entry.header().entry_type().is_symlink() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                rootfs.join(&relative),
                fs::Permissions::from_mode(sanitized_mode),
            )?;
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let path = rootfs.join(&relative);
            let metadata = fs::symlink_metadata(&path)?;
            let mode = metadata.mode();
            let temporarily_writable = !metadata.file_type().is_symlink() && mode & 0o200 == 0;
            if temporarily_writable {
                fs::set_permissions(&path, fs::Permissions::from_mode(mode | 0o200))?;
            }
            let override_stat = format!("{}:{}:0{:o}", linux_owner.0, linux_owner.1, mode);
            let xattr_result = persisting_overlay_core::sys::set_xattr(
                &path,
                std::ffi::OsStr::new("user.containers.override_stat"),
                override_stat.as_bytes(),
                0,
            );
            if temporarily_writable {
                fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
            }
            xattr_result?;
        }
        #[cfg(unix)]
        if let Some(mode) = directory_mode {
            use std::os::unix::fs::PermissionsExt;
            deferred_directory_modes.insert(relative.clone(), mode);
            fs::set_permissions(
                rootfs.join(&relative),
                fs::Permissions::from_mode(mode | 0o700),
            )?;
        }
    }
    #[cfg(unix)]
    restore_directory_modes(rootfs, deferred_directory_modes)?;
    Ok(())
}

#[cfg(unix)]
fn make_ancestor_directories_writable(
    rootfs: &Path,
    relative: &Path,
    deferred_modes: &mut BTreeMap<PathBuf, u32>,
) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut relative_directory = PathBuf::new();
    for component in relative
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .components()
    {
        relative_directory.push(component.as_os_str());
        let directory = rootfs.join(&relative_directory);
        let metadata = match fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "OCI layer entry traverses non-directory {}",
            directory.display()
        );
        let mode = metadata.permissions().mode() & 0o7777;
        deferred_modes
            .entry(relative_directory.clone())
            .or_insert(mode);
        if mode & 0o700 != 0o700 {
            fs::set_permissions(&directory, fs::Permissions::from_mode(mode | 0o700))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn restore_directory_modes(
    rootfs: &Path,
    deferred_modes: BTreeMap<PathBuf, u32>,
) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut directories = deferred_modes.into_iter().collect::<Vec<_>>();
    directories.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    for (relative, mode) in directories {
        let directory = rootfs.join(relative);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                fs::set_permissions(directory, fs::Permissions::from_mode(mode))?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

enum Whiteout {
    Remove(PathBuf),
    Opaque(PathBuf),
}

fn whiteout(path: &Path) -> Option<Whiteout> {
    let name = path.file_name()?.to_str()?;
    if name == ".wh..wh..opq" {
        return Some(Whiteout::Opaque(
            path.parent().unwrap_or_else(|| Path::new("")).to_path_buf(),
        ));
    }
    name.strip_prefix(".wh.")
        .map(|target| Whiteout::Remove(path.parent().unwrap_or_else(|| Path::new("")).join(target)))
}

fn clean_relative(path: &Path) -> anyhow::Result<PathBuf> {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => clean.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("unsafe OCI layer path {}", path.display())
            }
        }
    }
    Ok(clean)
}

fn ensure_no_symlink_ancestors(root: &Path, relative: &Path) -> anyhow::Result<()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("OCI whiteout traverses symlink {}", current.display())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn remove_relative(root: &Path, relative: &Path) -> anyhow::Result<()> {
    ensure_no_symlink_ancestors(root, relative.parent().unwrap_or_else(|| Path::new("")))?;
    let target = root.join(relative);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(target)?
        }
        Ok(_) => fs::remove_file(target)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn clear_relative_directory(root: &Path, relative: &Path) -> anyhow::Result<()> {
    ensure_no_symlink_ancestors(root, relative)?;
    let target = root.join(relative);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            for entry in fs::read_dir(target)? {
                let path = entry?.path();
                let metadata = fs::symlink_metadata(&path)?;
                if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    fs::remove_dir_all(path)?;
                } else {
                    fs::remove_file(path)?;
                }
            }
        }
        Ok(_) => bail!(
            "opaque OCI whiteout targets a non-directory: {}",
            target.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_docker_style_references() {
        assert_eq!(
            ImageReference::parse("ubuntu").unwrap(),
            ImageReference {
                registry: "registry-1.docker.io".into(),
                repository: "library/ubuntu".into(),
                reference: "latest".into(),
            }
        );
        assert_eq!(
            ImageReference::parse("ghcr.io/acme/agent:v1").unwrap(),
            ImageReference {
                registry: "ghcr.io".into(),
                repository: "acme/agent".into(),
                reference: "v1".into(),
            }
        );
        assert_eq!(
            ImageReference::parse("docker.io/ubuntu:24.04")
                .unwrap()
                .registry,
            "registry-1.docker.io"
        );
    }

    #[test]
    fn nullable_docker_runtime_fields_decode_as_empty() {
        let config: ImageConfiguration =
            serde_json::from_str(r#"{"config":{"Env":null,"Entrypoint":null,"Cmd":null}}"#)
                .unwrap();
        assert!(config.config.env.is_none());
        assert!(config.config.entrypoint.is_none());
        assert!(config.config.cmd.is_none());
    }

    #[test]
    fn bearer_challenge_is_parsed() {
        let fields = parse_bearer_challenge(
            r#"Bearer realm="https://auth.example/token",service="registry.example",scope="repository:acme/app:pull""#,
        )
        .unwrap();
        assert_eq!(fields["service"], "registry.example");
        assert_eq!(fields["scope"], "repository:acme/app:pull");
    }

    #[test]
    fn whiteouts_remove_previous_layer_entries() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("etc/sub")).unwrap();
        fs::write(root.path().join("etc/old"), b"old").unwrap();
        fs::write(root.path().join("etc/sub/old"), b"old").unwrap();
        remove_relative(root.path(), Path::new("etc/old")).unwrap();
        clear_relative_directory(root.path(), Path::new("etc/sub")).unwrap();
        assert!(!root.path().join("etc/old").exists());
        assert!(fs::read_dir(root.path().join("etc/sub"))
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn applies_an_uncompressed_oci_layer() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("etc")).unwrap();
        fs::write(root.path().join("etc/old"), b"old").unwrap();
        let layer = tempfile::NamedTempFile::new().unwrap();
        {
            let mut archive = tar::Builder::new(layer.reopen().unwrap());
            let mut whiteout = tar::Header::new_gnu();
            whiteout.set_mode(0o600);
            whiteout.set_size(0);
            whiteout.set_cksum();
            archive
                .append_data(&mut whiteout, "etc/.wh.old", std::io::empty())
                .unwrap();
            let body = b"new";
            let mut file = tar::Header::new_gnu();
            file.set_mode(0o644);
            file.set_size(body.len() as u64);
            file.set_cksum();
            archive
                .append_data(&mut file, "etc/new", body.as_slice())
                .unwrap();
            archive.finish().unwrap();
        }
        apply_layer(
            layer.path(),
            "application/vnd.oci.image.layer.v1.tar",
            root.path(),
        )
        .unwrap();
        assert!(!root.path().join("etc/old").exists());
        assert_eq!(fs::read(root.path().join("etc/new")).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn layer_can_populate_a_read_only_directory() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let layer = tempfile::NamedTempFile::new().unwrap();
        {
            let mut archive = tar::Builder::new(layer.reopen().unwrap());
            let mut directory = tar::Header::new_gnu();
            directory.set_entry_type(tar::EntryType::Directory);
            directory.set_mode(0o555);
            directory.set_size(0);
            directory.set_cksum();
            archive
                .append_data(&mut directory, "certs", std::io::empty())
                .unwrap();

            let body = b"certificate";
            let mut file = tar::Header::new_gnu();
            file.set_mode(0o444);
            file.set_size(body.len() as u64);
            file.set_cksum();
            archive
                .append_data(&mut file, "certs/root.pem", body.as_slice())
                .unwrap();

            let mut symlink = tar::Header::new_gnu();
            symlink.set_entry_type(tar::EntryType::Symlink);
            symlink.set_mode(0o777);
            symlink.set_size(0);
            archive
                .append_link(&mut symlink, "certs/hash.0", "root.pem")
                .unwrap();
            archive.finish().unwrap();
        }

        apply_layer(
            layer.path(),
            "application/vnd.oci.image.layer.v1.tar",
            root.path(),
        )
        .unwrap();

        assert_eq!(
            fs::read_link(root.path().join("certs/hash.0")).unwrap(),
            Path::new("root.pem")
        );
        assert_eq!(
            fs::metadata(root.path().join("certs"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o555
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rootless_layer_strips_setuid_and_setgid_bits() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let layer = tempfile::NamedTempFile::new().unwrap();
        {
            let mut archive = tar::Builder::new(layer.reopen().unwrap());
            let body = b"executable";
            let mut file = tar::Header::new_gnu();
            file.set_mode(0o6755);
            file.set_size(body.len() as u64);
            file.set_cksum();
            archive
                .append_data(&mut file, "bin/tool", body.as_slice())
                .unwrap();
            archive.finish().unwrap();
        }

        apply_layer(
            layer.path(),
            "application/vnd.oci.image.layer.v1.tar",
            root.path(),
        )
        .unwrap();

        assert_eq!(
            fs::metadata(root.path().join("bin/tool"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o755
        );
    }

    #[test]
    fn layer_paths_cannot_escape_root() {
        assert!(clean_relative(Path::new("../../host")).is_err());
        assert_eq!(
            clean_relative(Path::new("./etc/passwd")).unwrap(),
            Path::new("etc/passwd")
        );
    }
}
