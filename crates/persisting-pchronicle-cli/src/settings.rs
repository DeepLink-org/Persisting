use super::*;

const MAX_WAREHOUSE_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_WAREHOUSE_DATASETS: usize = 128;
const CONFIG_ENV: &str = "PCHRONICLE_CONFIG";
const LEGACY_SETTINGS_ENV: &str = "PCHRONICLE_SETTINGS";
const RESERVED_ALIASES: [&str; 3] = ["codex", "claude", "claude-code"];

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LocalSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_warehouse: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    aliases: BTreeMap<String, String>,
    /// Credentials are deliberately kept out of the alias URI so they cannot
    /// leak through `alias list`, logs, or generated Dataset paths.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    alias_credentials: BTreeMap<String, S3Credentials>,
    /// S3-compatible endpoints are kept separate from the canonical s3:// URI.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    alias_endpoints: BTreeMap<String, String>,
    /// Optional S3 regions are kept separate from the canonical s3:// URI.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    alias_regions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct S3Credentials {
    access_key: String,
    secret_key: String,
}

pub(super) fn default_settings_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(CONFIG_ENV).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os(LEGACY_SETTINGS_ENV).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.map(|base| base.join("pchronicle/config.toml"))
        .context("cannot locate the user configuration directory; pass --config <FILE>")
}

pub(super) fn settings_path(override_path: Option<&Path>) -> Result<PathBuf> {
    match override_path {
        Some(path) => Ok(path.to_path_buf()),
        None => default_settings_path(),
    }
}

fn load_local_settings(path: &Path) -> Result<LocalSettings> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("read pChronicle settings metadata {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "pChronicle settings must be a regular file"
    );
    anyhow::ensure!(
        metadata.len() <= MAX_WAREHOUSE_CONFIG_BYTES,
        "pChronicle settings exceed the {} byte limit",
        MAX_WAREHOUSE_CONFIG_BYTES
    );
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read pChronicle settings {}", path.display()))?;
    let settings: LocalSettings = toml::from_str(&content)
        .with_context(|| format!("parse pChronicle settings {}", path.display()))?;
    Ok(settings)
}

fn load_local_settings_or_default(path: &Path) -> Result<LocalSettings> {
    if path.exists() {
        load_local_settings(path)
    } else {
        Ok(LocalSettings::default())
    }
}

pub(super) fn resolve_default_warehouse(settings_override: Option<&Path>) -> Result<String> {
    let path = settings_path(settings_override)?;
    if !path.exists() {
        return Err(cli_boundary_error(
            BoundaryCode::NotFound,
            format!(
                "default Dataset is not configured; run `pchronicle default set <LOCAL_DATASET>` (config: {})",
                path.display()
            ),
        ));
    }
    let settings = load_local_settings(&path)?;
    let configured = settings.default_warehouse.as_deref().ok_or_else(|| {
        cli_boundary_error(
            BoundaryCode::NotFound,
            format!(
                "default Dataset is not configured; run `pchronicle default set <LOCAL_DATASET>` (config: {})",
                path.display()
            ),
        )
    })?;
    let warehouse = normalize_and_validate_dataset_uri(configured)
        .context("validate configured default Dataset")?;
    anyhow::ensure!(
        !warehouse.contains("://") && Path::new(&warehouse).is_dir(),
        "configured default Dataset must be a local directory"
    );
    Ok(warehouse)
}

fn write_local_settings(path: &Path, settings: &LocalSettings) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create pChronicle settings directory {}", parent.display()))?;
    anyhow::ensure!(
        parent.is_dir(),
        "pChronicle settings parent is not a directory"
    );
    if path.exists() {
        anyhow::ensure!(
            path.is_file(),
            "pChronicle settings path is not a regular file"
        );
    }
    let content = toml::to_string_pretty(settings).context("encode pChronicle settings")?;
    let mut staging = tempfile::Builder::new()
        .prefix(".pchronicle-settings-")
        .tempfile_in(parent)
        .context("create pChronicle settings staging file")?;
    staging
        .write_all(content.as_bytes())
        .context("write pChronicle settings staging file")?;
    staging
        .as_file()
        .sync_all()
        .context("sync pChronicle settings staging file")?;
    staging
        .persist(path)
        .map_err(|error| error.error)
        .context("publish pChronicle settings atomically")?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .context("sync pChronicle settings directory")?;
    Ok(())
}

pub(super) fn run_default(
    args: DefaultArgs,
    settings_override: Option<&Path>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let path = settings_path(settings_override)?;
    let command = match (args.command, args.legacy_directory) {
        (Some(command), None) => command,
        (None, Some(directory)) => DefaultCommand::Set {
            dataset: directory.to_string_lossy().into_owned(),
        },
        (None, None) => DefaultCommand::Show,
        (Some(_), Some(_)) => unreachable!("clap rejects mixed default forms"),
    };
    match command {
        DefaultCommand::Show => {
            let warehouse = resolve_default_warehouse(settings_override)?;
            writeln!(stdout, "{warehouse}").context("write default Dataset")
        }
        DefaultCommand::Set { dataset } => {
            let expanded = expand_dataset_reference(&dataset, settings_override, false)?;
            let location = DatasetLocation::parse(&expanded)?;
            let directory = location
                .local_path()
                .context("default Dataset must be a local directory")?;
            if !directory.exists() {
                std::fs::create_dir_all(directory).with_context(|| {
                    format!("create default Dataset directory {}", directory.display())
                })?;
            }
            anyhow::ensure!(directory.is_dir(), "default Dataset must be a directory");
            let warehouse = std::fs::canonicalize(directory)
                .context("canonicalize default Dataset directory")?
                .to_string_lossy()
                .into_owned();
            let mut settings = load_local_settings_or_default(&path)?;
            settings.default_warehouse = Some(warehouse.clone());
            write_local_settings(&path, &settings)?;
            writeln!(stderr, "config={} updated=true", path.display())
                .context("write pChronicle default metadata")?;
            writeln!(stdout, "{warehouse}").context("write default Dataset")
        }
        DefaultCommand::Clear => {
            let mut settings = load_local_settings_or_default(&path)?;
            settings.default_warehouse = None;
            write_local_settings(&path, &settings)?;
            writeln!(stderr, "config={} updated=true", path.display())
                .context("write pChronicle default metadata")?;
            writeln!(stdout, "cleared").context("write default clear result")
        }
    }
}

#[derive(Serialize)]
struct AliasListResponse<'a> {
    schema_version: &'static str,
    aliases: Vec<AliasResponse<'a>>,
}

#[derive(Serialize)]
struct AliasResponse<'a> {
    name: &'a str,
    dataset: &'a str,
}

pub(super) fn run_alias(
    args: AliasArgs,
    settings_override: Option<&Path>,
    stdout_is_terminal: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let path = settings_path(settings_override)?;
    let mut settings = load_local_settings_or_default(&path)?;
    match args.command.unwrap_or(AliasCommand::List {
        format: OutputFormat::Auto,
    }) {
        AliasCommand::List { format } => {
            let format = match format {
                OutputFormat::Auto if stdout_is_terminal => OutputFormat::Table,
                OutputFormat::Auto => OutputFormat::Json,
                explicit => explicit,
            };
            match format {
                OutputFormat::Table => {
                    writeln!(stdout, "NAME\tDATASET")?;
                    for (name, dataset) in alias_list_entries(&settings)? {
                        writeln!(stdout, "{name}\t{dataset}")?;
                    }
                }
                OutputFormat::Json => {
                    let entries = alias_list_entries(&settings)?;
                    let response = AliasListResponse {
                        schema_version: "pchronicle-aliases/v1",
                        aliases: entries
                            .iter()
                            .map(|(name, dataset)| AliasResponse { name, dataset })
                            .collect(),
                    };
                    serde_json::to_writer_pretty(&mut *stdout, &response)?;
                    writeln!(stdout)?;
                }
                OutputFormat::Auto => unreachable!(),
            }
            Ok(())
        }
        AliasCommand::Add {
            name,
            dataset,
            endpoint,
            region,
            access_key,
            secret_key,
        } => {
            validate_alias_name(&name)?;
            if settings.aliases.contains_key(&name) {
                return Err(cli_boundary_error(
                    BoundaryCode::Conflict,
                    format!("alias '{name}' already exists"),
                ));
            }
            let dataset = normalize_alias_target(&dataset)?;
            let endpoint = s3_endpoint_for(&dataset, endpoint)?;
            let region = s3_region_for(&dataset, region)?;
            let credentials = s3_credentials_for(&dataset, access_key, secret_key)?;
            settings.aliases.insert(name.clone(), dataset.clone());
            if let Some(endpoint) = endpoint {
                settings.alias_endpoints.insert(name.clone(), endpoint);
            }
            if let Some(region) = region {
                settings.alias_regions.insert(name.clone(), region);
            }
            if let Some(credentials) = credentials {
                settings.alias_credentials.insert(name.clone(), credentials);
            }
            write_local_settings(&path, &settings)?;
            writeln!(stderr, "config={} updated=true", path.display())?;
            writeln!(stdout, "{name}\t{dataset}")?;
            Ok(())
        }
        AliasCommand::GetUrl { name } => {
            validate_alias_name(&name)?;
            let dataset = settings.aliases.get(&name).ok_or_else(|| {
                cli_boundary_error(
                    BoundaryCode::NotFound,
                    format!("alias '{name}' does not exist"),
                )
            })?;
            writeln!(stdout, "{dataset}")?;
            Ok(())
        }
        AliasCommand::SetUrl {
            name,
            dataset,
            endpoint,
            region,
            access_key,
            secret_key,
        } => {
            validate_alias_name(&name)?;
            if !settings.aliases.contains_key(&name) {
                return Err(cli_boundary_error(
                    BoundaryCode::NotFound,
                    format!("alias '{name}' does not exist"),
                ));
            }
            let dataset = normalize_alias_target(&dataset)?;
            let endpoint = s3_endpoint_for(&dataset, endpoint)?;
            let region = s3_region_for(&dataset, region)?;
            let credentials = s3_credentials_for(&dataset, access_key, secret_key)?;
            settings.aliases.insert(name.clone(), dataset.clone());
            match endpoint {
                Some(endpoint) => {
                    settings.alias_endpoints.insert(name.clone(), endpoint);
                }
                None if !dataset.starts_with("s3://") => {
                    settings.alias_endpoints.remove(&name);
                }
                None => {}
            }
            match region {
                Some(region) => {
                    settings.alias_regions.insert(name.clone(), region);
                }
                None if !dataset.starts_with("s3://") => {
                    settings.alias_regions.remove(&name);
                }
                None => {}
            }
            match credentials {
                Some(credentials) => {
                    settings.alias_credentials.insert(name.clone(), credentials);
                }
                None if !dataset.starts_with("s3://") => {
                    settings.alias_credentials.remove(&name);
                }
                None => {}
            }
            write_local_settings(&path, &settings)?;
            writeln!(stderr, "config={} updated=true", path.display())?;
            writeln!(stdout, "{name}\t{dataset}")?;
            Ok(())
        }
        AliasCommand::Rename { old, new } => {
            validate_alias_name(&old)?;
            validate_alias_name(&new)?;
            if settings.aliases.contains_key(&new) {
                return Err(cli_boundary_error(
                    BoundaryCode::Conflict,
                    format!("alias '{new}' already exists"),
                ));
            }
            let dataset = settings.aliases.remove(&old).ok_or_else(|| {
                cli_boundary_error(
                    BoundaryCode::NotFound,
                    format!("alias '{old}' does not exist"),
                )
            })?;
            settings.aliases.insert(new.clone(), dataset);
            if let Some(credentials) = settings.alias_credentials.remove(&old) {
                settings.alias_credentials.insert(new.clone(), credentials);
            }
            if let Some(endpoint) = settings.alias_endpoints.remove(&old) {
                settings.alias_endpoints.insert(new.clone(), endpoint);
            }
            if let Some(region) = settings.alias_regions.remove(&old) {
                settings.alias_regions.insert(new.clone(), region);
            }
            write_local_settings(&path, &settings)?;
            writeln!(stderr, "config={} updated=true", path.display())?;
            writeln!(stdout, "{new}")?;
            Ok(())
        }
        AliasCommand::Remove { name } => {
            validate_alias_name(&name)?;
            if settings.aliases.remove(&name).is_none() {
                return Err(cli_boundary_error(
                    BoundaryCode::NotFound,
                    format!("alias '{name}' does not exist"),
                ));
            }
            settings.alias_credentials.remove(&name);
            settings.alias_endpoints.remove(&name);
            settings.alias_regions.remove(&name);
            write_local_settings(&path, &settings)?;
            writeln!(stderr, "config={} updated=true", path.display())?;
            writeln!(stdout, "{name}")?;
            Ok(())
        }
    }
}

fn alias_list_entries(settings: &LocalSettings) -> Result<Vec<(String, String)>> {
    let mut entries = Vec::with_capacity(settings.aliases.len() + RESERVED_ALIASES.len());
    for name in RESERVED_ALIASES {
        let dataset = expand_dataset_alias(&format!("@{name}"))?;
        entries.push((format!("@{name}"), dataset));
    }
    entries.extend(
        settings
            .aliases
            .iter()
            .map(|(name, dataset)| (name.clone(), dataset.clone())),
    );
    Ok(entries)
}

fn validate_alias_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let first = chars.next();
    let valid = name.len() <= 64
        && first.is_some_and(|character| character.is_ascii_lowercase())
        && chars.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | '-')
        });
    if !valid {
        return Err(cli_boundary_error(
            BoundaryCode::InvalidRequest,
            "alias name must match [a-z][a-z0-9._-]{0,63}",
        ));
    }
    if RESERVED_ALIASES.contains(&name) {
        return Err(cli_boundary_error(
            BoundaryCode::InvalidRequest,
            format!("alias name '{name}' is reserved"),
        ));
    }
    Ok(())
}

fn normalize_alias_target(dataset: &str) -> Result<String> {
    let dataset = dataset.trim();
    anyhow::ensure!(
        !dataset.starts_with('@'),
        "an alias cannot point to another alias"
    );
    let location = DatasetLocation::parse(dataset)?;
    if location.is_object_store() || dataset.contains("://") {
        return Ok(location.as_str().to_owned());
    }
    let path = location
        .local_path()
        .context("local alias target has no path")?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("locate current directory for alias target")?
            .join(path)
    };
    Ok(absolute.to_string_lossy().into_owned())
}

fn s3_credentials_for(
    dataset: &str,
    access_key: Option<String>,
    secret_key: Option<String>,
) -> Result<Option<S3Credentials>> {
    match (access_key, secret_key) {
        (None, None) => Ok(None),
        (Some(access_key), Some(secret_key)) => {
            anyhow::ensure!(
                dataset.starts_with("s3://"),
                "--ak/--sk can only be used with an s3:// Dataset"
            );
            let access_key = access_key.trim().to_string();
            let secret_key = secret_key.trim().to_string();
            anyhow::ensure!(!access_key.is_empty(), "S3 access key must not be empty");
            anyhow::ensure!(!secret_key.is_empty(), "S3 secret key must not be empty");
            Ok(Some(S3Credentials {
                access_key,
                secret_key,
            }))
        }
        _ => unreachable!("clap requires --ak and --sk together"),
    }
}

pub(super) fn s3_endpoint_for(dataset: &str, endpoint: Option<String>) -> Result<Option<String>> {
    let Some(endpoint) = endpoint else {
        return Ok(None);
    };
    anyhow::ensure!(
        dataset.starts_with("s3://"),
        "--endpoint can only be used with an s3:// Dataset"
    );
    let endpoint = endpoint.trim().trim_end_matches('/').to_owned();
    anyhow::ensure!(!endpoint.is_empty(), "S3 endpoint must not be empty");
    anyhow::ensure!(
        !endpoint.starts_with('[') && !endpoint.contains("](") && !endpoint.ends_with(')'),
        "S3 endpoint must be a plain URL, not a Markdown link"
    );
    let parsed = url::Url::parse(&endpoint).context("parse S3 endpoint URL")?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "http" | "https"),
        "S3 endpoint must use http:// or https://"
    );
    anyhow::ensure!(
        parsed.host_str().is_some(),
        "S3 endpoint must include a host"
    );
    anyhow::ensure!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "S3 endpoint must not contain embedded credentials"
    );
    anyhow::ensure!(
        parsed.query().is_none() && parsed.fragment().is_none(),
        "S3 endpoint must not contain a query string or fragment"
    );
    Ok(Some(endpoint))
}

fn s3_region_for(dataset: &str, region: Option<String>) -> Result<Option<String>> {
    let Some(region) = region else {
        return Ok(None);
    };
    anyhow::ensure!(
        dataset.starts_with("s3://"),
        "--region can only be used with an s3:// Dataset"
    );
    let region = region.trim().to_owned();
    anyhow::ensure!(!region.is_empty(), "S3 region must not be empty");
    anyhow::ensure!(
        region.len() <= 128 && !region.chars().any(char::is_whitespace),
        "S3 region must be a non-empty region name without whitespace"
    );
    Ok(Some(region))
}

fn apply_alias_credentials(settings: &LocalSettings, name: &str) {
    let Some(credentials) = settings.alias_credentials.get(name) else {
        return;
    };
    // Object-store clients read these standard variables when opening the
    // resolved S3 URI. The values are never included in the URI or output.
    unsafe {
        std::env::set_var("AWS_ACCESS_KEY_ID", &credentials.access_key);
        std::env::set_var("AWS_SECRET_ACCESS_KEY", &credentials.secret_key);
    }
}

fn apply_alias_endpoint(settings: &LocalSettings, name: &str) {
    let Some(endpoint) = settings.alias_endpoints.get(name) else {
        return;
    };
    // Set both names: Lance uses the generic endpoint key to skip AWS region
    // discovery, while object_store also recognizes the S3-specific spelling.
    unsafe {
        std::env::set_var("AWS_ENDPOINT", endpoint);
        std::env::set_var("AWS_ENDPOINT_URL_S3", endpoint);
        if endpoint.starts_with("http://") {
            // object_store rejects plaintext HTTP by default. Local MinIO and
            // other development S3-compatible services commonly use it.
            std::env::set_var("AWS_ALLOW_HTTP", "true");
        }
    }
}

fn apply_alias_region(settings: &LocalSettings, name: &str) {
    let Some(region) = settings.alias_regions.get(name) else {
        return;
    };
    unsafe {
        std::env::set_var("AWS_REGION", region);
    }
}

pub(super) fn expand_dataset_reference(
    input: &str,
    settings_override: Option<&Path>,
    require_existing: bool,
) -> Result<String> {
    let input = input.trim();
    let expanded = if !input.starts_with('@') {
        input.to_owned()
    } else {
        let rest = &input[1..];
        let (name, suffix) = rest.split_once('/').unwrap_or((rest, ""));
        validate_alias_suffix(suffix)?;
        if RESERVED_ALIASES.contains(&name) {
            expand_dataset_alias(input)?
        } else {
            validate_alias_name(name)?;
            let path = settings_path(settings_override)?;
            let settings = load_local_settings_or_default(&path)?;
            let root = settings.aliases.get(name).ok_or_else(|| {
                cli_boundary_error(
                    BoundaryCode::NotFound,
                    format!("unknown Dataset alias '@{name}'"),
                )
            })?;
            let expanded = join_alias_target(root, suffix)?;
            apply_alias_credentials(&settings, name);
            apply_alias_endpoint(&settings, name);
            apply_alias_region(&settings, name);
            expanded
        }
    };
    let location = DatasetLocation::parse(&expanded)?;
    if require_existing {
        if location.local_path().is_some_and(|path| !path.exists()) {
            return Err(cli_boundary_error(
                BoundaryCode::NotFound,
                format!("Dataset does not exist: {}", location.as_str()),
            ));
        }
        Ok(location.into_existing()?.as_str().to_owned())
    } else {
        Ok(location.as_str().to_owned())
    }
}

fn validate_alias_suffix(suffix: &str) -> Result<()> {
    if suffix.is_empty() {
        return Ok(());
    }
    anyhow::ensure!(
        !suffix.contains(['\\', '\0']),
        "alias suffix contains an invalid character"
    );
    for component in suffix.split('/') {
        anyhow::ensure!(
            !component.is_empty() && component != "." && component != "..",
            "alias suffix must not contain empty, '.', or '..' segments"
        );
    }
    Ok(())
}

fn join_alias_target(root: &str, suffix: &str) -> Result<String> {
    if suffix.is_empty() {
        return Ok(root.to_owned());
    }
    let location = DatasetLocation::parse(root)?;
    match location.local_path() {
        Some(path) => Ok(path.join(suffix).to_string_lossy().into_owned()),
        None => Ok(format!("{}/{}", root.trim_end_matches('/'), suffix)),
    }
}

pub(super) fn resolve_dataset_uri(
    explicit: Option<&str>,
    settings_override: Option<&Path>,
) -> Result<String> {
    match explicit {
        Some(uri) => expand_dataset_reference(uri, settings_override, true),
        None => resolve_default_warehouse(settings_override),
    }
}

pub(super) fn default_import_output(
    args: &ImportArgs,
    settings_override: Option<&Path>,
) -> Result<String> {
    anyhow::ensure!(
        !args.stream,
        "stream import requires an explicit --output Dataset"
    );
    let file_name = Path::new(&args.from)
        .file_name()
        .and_then(|name| name.to_str())
        .context("import input must have a UTF-8 file name")?;
    let stem = file_name.strip_suffix(".json").unwrap_or(file_name);
    let stem = stem.strip_suffix(".actf").unwrap_or(stem);
    let mut dataset_name = stem
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while dataset_name.contains("--") {
        dataset_name = dataset_name.replace("--", "-");
    }
    let dataset_name = dataset_name.trim_matches('-');
    anyhow::ensure!(
        !dataset_name.is_empty(),
        "cannot derive Dataset name from import input"
    );
    let warehouse = resolve_default_warehouse(settings_override)?;
    Ok(Path::new(&warehouse)
        .join(dataset_name)
        .to_string_lossy()
        .into_owned())
}

pub(super) fn load_warehouse_config_with_user_config(
    path: &Path,
    settings_override: Option<&Path>,
) -> Result<server::ChronicleServerConfig> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("read Warehouse config metadata {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "Warehouse config must be a regular file"
    );
    anyhow::ensure!(
        metadata.len() <= MAX_WAREHOUSE_CONFIG_BYTES,
        "Warehouse config exceeds the {} byte limit",
        MAX_WAREHOUSE_CONFIG_BYTES
    );
    let mut content = String::new();
    std::fs::File::open(path)
        .with_context(|| format!("open Warehouse config {}", path.display()))?
        .take(MAX_WAREHOUSE_CONFIG_BYTES + 1)
        .read_to_string(&mut content)
        .with_context(|| format!("read Warehouse config {}", path.display()))?;
    anyhow::ensure!(
        content.len() as u64 <= MAX_WAREHOUSE_CONFIG_BYTES,
        "Warehouse config exceeds the {} byte limit",
        MAX_WAREHOUSE_CONFIG_BYTES
    );
    let file: WarehouseFile = toml::from_str(&content)
        .with_context(|| format!("parse Warehouse config {}", path.display()))?;
    anyhow::ensure!(!file.datasets.is_empty(), "mount at least one Dataset");
    anyhow::ensure!(
        file.datasets.len() <= MAX_WAREHOUSE_DATASETS,
        "Warehouse config mounts more than {MAX_WAREHOUSE_DATASETS} Datasets"
    );

    let mut names = HashSet::with_capacity(file.datasets.len());
    let mut mounts = Vec::with_capacity(file.datasets.len());
    let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
    for dataset in file.datasets {
        let input = if dataset.uri.trim_start().starts_with('@') {
            dataset.uri
        } else if !dataset.uri.contains("://") && Path::new(&dataset.uri).is_relative() {
            config_dir.join(&dataset.uri).to_string_lossy().into_owned()
        } else {
            dataset.uri
        };
        let uri = expand_dataset_reference(&input, settings_override, true)
            .with_context(|| format!("validate Dataset '{}'", dataset.name))?;
        let mount = DatasetMount::new(dataset.name, uri)?;
        anyhow::ensure!(
            names.insert(mount.name.clone()),
            "Dataset names must be unique; duplicate '{}'",
            mount.name
        );
        mounts.push(mount);
    }

    let mut config = server::ChronicleServerConfig::mounted(mounts)?;
    if let Some(default_dataset) = file.default_dataset {
        let normalized = DatasetMount::new(default_dataset, "validation")?.name;
        anyhow::ensure!(
            names.contains(&normalized),
            "default_dataset '{normalized}' is not mounted"
        );
        config.default_dataset = Some(normalized);
    }
    config.catalog_options.error_policy = CatalogErrorPolicy::Report;
    Ok(config)
}

#[cfg(test)]
pub(super) fn load_warehouse_config(path: &Path) -> Result<server::ChronicleServerConfig> {
    load_warehouse_config_with_user_config(path, None)
}
