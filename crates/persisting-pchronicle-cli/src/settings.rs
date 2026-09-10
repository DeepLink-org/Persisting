use super::*;

const MAX_WAREHOUSE_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_WAREHOUSE_DATASETS: usize = 128;
const CONFIG_ENV: &str = "PCHRONICLE_CONFIG";
const RESERVED_PIN_NAMES: [&str; 3] = ["codex", "claude", "claude-code"];
const DEFAULT_PIN_NAME: &str = "default";

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LocalSettings {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pins: BTreeMap<String, PinConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PinConfig {
    uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret_key: Option<String>,
}

impl PinConfig {
    fn new_uri(uri: String) -> Self {
        Self {
            uri,
            endpoint: None,
            region: None,
            access_key: None,
            secret_key: None,
        }
    }

    fn credentials_pair(&self) -> Result<Option<(&str, &str)>> {
        match (&self.access_key, &self.secret_key) {
            (None, None) => Ok(None),
            (Some(access_key), Some(secret_key)) => {
                anyhow::ensure!(!access_key.is_empty(), "pin access_key must not be empty");
                anyhow::ensure!(!secret_key.is_empty(), "pin secret_key must not be empty");
                Ok(Some((access_key.as_str(), secret_key.as_str())))
            }
            _ => Err(cli_boundary_error(
                BoundaryCode::InvalidRequest,
                "pin access_key and secret_key must be set together",
            )),
        }
    }
}

pub(super) fn default_settings_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(CONFIG_ENV).filter(|value| !value.is_empty()) {
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
    validate_loaded_settings(&settings)?;
    Ok(settings)
}

fn validate_loaded_settings(settings: &LocalSettings) -> Result<()> {
    for (name, pin) in &settings.pins {
        if name != DEFAULT_PIN_NAME {
            validate_pin_name(name)?;
        }
        let _ = pin.credentials_pair()?;
    }
    Ok(())
}

fn load_local_settings_or_default(path: &Path) -> Result<LocalSettings> {
    if path.exists() {
        load_local_settings(path)
    } else {
        Ok(LocalSettings::default())
    }
}

pub(super) fn resolve_default_pin(settings_override: Option<&Path>) -> Result<String> {
    let path = settings_path(settings_override)?;
    if !path.exists() {
        return Err(cli_boundary_error(
            BoundaryCode::NotFound,
            format!(
                "default Dataset is not configured; run `pchronicle dataset pin default <LOCAL_DATASET>` (config: {})",
                path.display()
            ),
        ));
    }
    let settings = load_local_settings(&path)?;
    let configured = settings
        .pins
        .get(DEFAULT_PIN_NAME)
        .map(|pin| pin.uri.as_str())
        .ok_or_else(|| {
        cli_boundary_error(
            BoundaryCode::NotFound,
            format!(
                "default Dataset is not configured; run `pchronicle dataset pin default <LOCAL_DATASET>` (config: {})",
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

#[derive(Serialize)]
struct PinListResponse<'a> {
    schema_version: &'static str,
    pins: Vec<PinResponse<'a>>,
}

#[derive(Serialize)]
struct PinResponse<'a> {
    name: &'a str,
    dataset: &'a str,
}

pub(super) fn run_dataset(
    args: DatasetArgs,
    settings_override: Option<&Path>,
    stdout_is_terminal: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let path = settings_path(settings_override)?;
    let mut settings = load_local_settings_or_default(&path)?;
    match args.command.unwrap_or(DatasetCommand::List {
        format: OutputFormat::Auto,
    }) {
        DatasetCommand::List { format } => {
            let format = match format {
                OutputFormat::Auto if stdout_is_terminal => OutputFormat::Table,
                OutputFormat::Auto => OutputFormat::Json,
                explicit => explicit,
            };
            match format {
                OutputFormat::Table => {
                    writeln!(stdout, "NAME\tDATASET")?;
                    for (name, dataset) in pin_list_entries(&settings)? {
                        writeln!(stdout, "{name}\t{dataset}")?;
                    }
                }
                OutputFormat::Json => {
                    let entries = pin_list_entries(&settings)?;
                    let response = PinListResponse {
                        schema_version: "pchronicle-dataset-pins/v1",
                        pins: entries
                            .iter()
                            .map(|(name, dataset)| PinResponse { name, dataset })
                            .collect(),
                    };
                    serde_json::to_writer_pretty(&mut *stdout, &response)?;
                    writeln!(stdout)?;
                }
                OutputFormat::Auto => unreachable!(),
            }
            Ok(())
        }
        DatasetCommand::Pin {
            name,
            dataset,
            endpoint,
            region,
            access_key,
            secret_key,
        } => {
            if name == DEFAULT_PIN_NAME {
                anyhow::ensure!(
                    endpoint.is_none()
                        && region.is_none()
                        && access_key.is_none()
                        && secret_key.is_none(),
                    "default pin is a local Dataset and does not accept --endpoint, --region, --ak, or --sk"
                );
                return pin_default_dataset(&path, &dataset, settings_override, stdout, stderr);
            }
            validate_pin_name(&name)?;
            if settings.pins.contains_key(&name) {
                return Err(cli_boundary_error(
                    BoundaryCode::Conflict,
                    format!("dataset pin '{name}' already exists"),
                ));
            }
            let pin = build_pin_config(&dataset, endpoint, region, access_key, secret_key)?;
            let uri = pin.uri.clone();
            settings.pins.insert(name.clone(), pin);
            write_local_settings(&path, &settings)?;
            writeln!(stderr, "config={} updated=true", path.display())?;
            writeln!(stdout, "{name}\t{uri}")?;
            Ok(())
        }
        DatasetCommand::Show { name } => {
            if name == DEFAULT_PIN_NAME {
                let warehouse = resolve_default_pin(settings_override)?;
                writeln!(stdout, "{warehouse}")?;
                return Ok(());
            }
            validate_pin_name(&name)?;
            let pin = settings.pins.get(&name).ok_or_else(|| {
                cli_boundary_error(
                    BoundaryCode::NotFound,
                    format!("dataset pin '{name}' does not exist"),
                )
            })?;
            writeln!(stdout, "{}", pin.uri)?;
            Ok(())
        }
        DatasetCommand::Set {
            name,
            dataset,
            endpoint,
            region,
            access_key,
            secret_key,
        } => {
            if name == DEFAULT_PIN_NAME {
                anyhow::ensure!(
                    endpoint.is_none()
                        && region.is_none()
                        && access_key.is_none()
                        && secret_key.is_none(),
                    "default pin is a local Dataset and does not accept --endpoint, --region, --ak, or --sk"
                );
                return pin_default_dataset(&path, &dataset, settings_override, stdout, stderr);
            }
            validate_pin_name(&name)?;
            let existing = settings.pins.get(&name).ok_or_else(|| {
                cli_boundary_error(
                    BoundaryCode::NotFound,
                    format!("dataset pin '{name}' does not exist"),
                )
            })?;
            let pin =
                merge_pin_config(existing, &dataset, endpoint, region, access_key, secret_key)?;
            let uri = pin.uri.clone();
            settings.pins.insert(name.clone(), pin);
            write_local_settings(&path, &settings)?;
            writeln!(stderr, "config={} updated=true", path.display())?;
            writeln!(stdout, "{name}\t{uri}")?;
            Ok(())
        }
        DatasetCommand::Rename { old, new } => {
            anyhow::ensure!(
                old != DEFAULT_PIN_NAME && new != DEFAULT_PIN_NAME,
                "the default pin cannot be renamed; unpin it or pin default to a new path"
            );
            validate_pin_name(&old)?;
            validate_pin_name(&new)?;
            if settings.pins.contains_key(&new) {
                return Err(cli_boundary_error(
                    BoundaryCode::Conflict,
                    format!("dataset pin '{new}' already exists"),
                ));
            }
            let pin = settings.pins.remove(&old).ok_or_else(|| {
                cli_boundary_error(
                    BoundaryCode::NotFound,
                    format!("dataset pin '{old}' does not exist"),
                )
            })?;
            settings.pins.insert(new.clone(), pin);
            write_local_settings(&path, &settings)?;
            writeln!(stderr, "config={} updated=true", path.display())?;
            writeln!(stdout, "{new}")?;
            Ok(())
        }
        DatasetCommand::Unpin { name } => {
            if name == DEFAULT_PIN_NAME {
                if settings.pins.remove(DEFAULT_PIN_NAME).is_none() {
                    return Err(cli_boundary_error(
                        BoundaryCode::NotFound,
                        "dataset pin 'default' does not exist",
                    ));
                }
                write_local_settings(&path, &settings)?;
                writeln!(stderr, "config={} updated=true", path.display())?;
                writeln!(stdout, "cleared")?;
                return Ok(());
            }
            validate_pin_name(&name)?;
            if settings.pins.remove(&name).is_none() {
                return Err(cli_boundary_error(
                    BoundaryCode::NotFound,
                    format!("dataset pin '{name}' does not exist"),
                ));
            }
            write_local_settings(&path, &settings)?;
            writeln!(stderr, "config={} updated=true", path.display())?;
            writeln!(stdout, "{name}")?;
            Ok(())
        }
    }
}

fn build_pin_config(
    dataset: &str,
    endpoint: Option<String>,
    region: Option<String>,
    access_key: Option<String>,
    secret_key: Option<String>,
) -> Result<PinConfig> {
    let uri = normalize_pin_target(dataset)?;
    let catalog = uri.starts_with("catalog://");
    anyhow::ensure!(
        !catalog || (endpoint.is_none() && region.is_none()),
        "catalog pins do not accept --endpoint or --region"
    );
    let endpoint = s3_endpoint_for(&uri, endpoint)?;
    let region = s3_region_for(&uri, region)?;
    let credentials = s3_credentials_for(&uri, access_key, secret_key)?;
    anyhow::ensure!(
        !catalog || credentials.is_some(),
        "catalog pins require --ak and --sk"
    );
    Ok(PinConfig {
        uri,
        endpoint,
        region,
        access_key: credentials.as_ref().map(|value| value.access_key.clone()),
        secret_key: credentials.as_ref().map(|value| value.secret_key.clone()),
    })
}

fn merge_pin_config(
    existing: &PinConfig,
    dataset: &str,
    endpoint: Option<String>,
    region: Option<String>,
    access_key: Option<String>,
    secret_key: Option<String>,
) -> Result<PinConfig> {
    let uri = normalize_pin_target(dataset)?;
    let catalog = uri.starts_with("catalog://");
    anyhow::ensure!(
        !catalog || (endpoint.is_none() && region.is_none()),
        "catalog pins do not accept --endpoint or --region"
    );
    let mut pin = PinConfig::new_uri(uri.clone());
    pin.endpoint = match s3_endpoint_for(&uri, endpoint)? {
        Some(endpoint) => Some(endpoint),
        None if !uri.starts_with("s3://") => None,
        None => existing.endpoint.clone(),
    };
    pin.region = match s3_region_for(&uri, region)? {
        Some(region) => Some(region),
        None if !uri.starts_with("s3://") => None,
        None => existing.region.clone(),
    };
    match s3_credentials_for(&uri, access_key, secret_key)? {
        Some(credentials) => {
            pin.access_key = Some(credentials.access_key);
            pin.secret_key = Some(credentials.secret_key);
        }
        None if !uri.starts_with("s3://") && !catalog => {
            pin.access_key = None;
            pin.secret_key = None;
        }
        None => {
            pin.access_key = existing.access_key.clone();
            pin.secret_key = existing.secret_key.clone();
        }
    }
    anyhow::ensure!(
        !catalog || pin.credentials_pair()?.is_some(),
        "catalog pins require --ak and --sk"
    );
    Ok(pin)
}

fn pin_default_dataset(
    path: &Path,
    dataset: &str,
    settings_override: Option<&Path>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let expanded = expand_dataset_reference(dataset, settings_override, false)?;
    let location = DatasetLocation::parse(&expanded)?;
    let directory = location
        .local_path()
        .context("default Dataset must be a local directory")?;
    if !directory.exists() {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("create default Dataset directory {}", directory.display()))?;
    }
    anyhow::ensure!(directory.is_dir(), "default Dataset must be a directory");
    let warehouse = std::fs::canonicalize(directory)
        .context("canonicalize default Dataset directory")?
        .to_string_lossy()
        .into_owned();
    let mut settings = load_local_settings_or_default(path)?;
    settings.pins.insert(
        DEFAULT_PIN_NAME.to_owned(),
        PinConfig::new_uri(warehouse.clone()),
    );
    write_local_settings(path, &settings)?;
    writeln!(stderr, "config={} updated=true", path.display())
        .context("write pChronicle default metadata")?;
    writeln!(stdout, "{warehouse}").context("write default Dataset")
}

fn pin_list_entries(settings: &LocalSettings) -> Result<Vec<(String, String)>> {
    let mut entries = Vec::with_capacity(settings.pins.len() + RESERVED_PIN_NAMES.len());
    if let Some(default) = settings.pins.get(DEFAULT_PIN_NAME) {
        entries.push((DEFAULT_PIN_NAME.to_owned(), default.uri.clone()));
    }
    for name in RESERVED_PIN_NAMES {
        let dataset = expand_builtin_pin(&format!("@{name}"))?;
        entries.push((format!("@{name}"), dataset));
    }
    for (name, pin) in &settings.pins {
        if name == DEFAULT_PIN_NAME {
            continue;
        }
        entries.push((name.clone(), pin.uri.clone()));
    }
    Ok(entries)
}

fn validate_pin_name(name: &str) -> Result<()> {
    if name == DEFAULT_PIN_NAME {
        return Err(cli_boundary_error(
            BoundaryCode::InvalidRequest,
            "pin name 'default' is reserved for the default Dataset; use `dataset pin default <LOCAL_DATASET>`",
        ));
    }
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
            "pin name must match [a-z][a-z0-9._-]{0,63}",
        ));
    }
    if RESERVED_PIN_NAMES.contains(&name) {
        return Err(cli_boundary_error(
            BoundaryCode::InvalidRequest,
            format!("pin name '{name}' is reserved"),
        ));
    }
    Ok(())
}

fn normalize_pin_target(dataset: &str) -> Result<String> {
    let dataset = dataset.trim();
    anyhow::ensure!(
        !dataset.starts_with('@'),
        "a pin cannot point to another pin"
    );
    if dataset.starts_with("catalog://") {
        return crate::server::catalog::parse_catalog_pin_target(dataset);
    }
    let location = DatasetLocation::parse(dataset)?;
    if location.is_object_store() || dataset.contains("://") {
        return Ok(location.as_str().to_owned());
    }
    let path = location
        .local_path()
        .context("local pin target has no path")?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("locate current directory for pin target")?
            .join(path)
    };
    Ok(absolute.to_string_lossy().into_owned())
}

#[derive(Debug, Clone)]
struct PinCredentials {
    access_key: String,
    secret_key: String,
}

fn s3_credentials_for(
    dataset: &str,
    access_key: Option<String>,
    secret_key: Option<String>,
) -> Result<Option<PinCredentials>> {
    match (access_key, secret_key) {
        (None, None) => Ok(None),
        (Some(access_key), Some(secret_key)) => {
            anyhow::ensure!(
                dataset.starts_with("s3://") || dataset.starts_with("catalog://"),
                "--ak/--sk can only be used with an s3:// Dataset or a catalog:// pin"
            );
            let access_key = access_key.trim().to_string();
            let secret_key = secret_key.trim().to_string();
            anyhow::ensure!(!access_key.is_empty(), "S3 access key must not be empty");
            anyhow::ensure!(!secret_key.is_empty(), "S3 secret key must not be empty");
            Ok(Some(PinCredentials {
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

fn apply_pin_backend_env(pin: &PinConfig) {
    if let Ok(Some((access_key, secret_key))) = pin.credentials_pair() {
        unsafe {
            std::env::set_var("AWS_ACCESS_KEY_ID", access_key);
            std::env::set_var("AWS_SECRET_ACCESS_KEY", secret_key);
        }
    }
    if let Some(endpoint) = pin.endpoint.as_deref() {
        unsafe {
            std::env::set_var("AWS_ENDPOINT", endpoint);
            std::env::set_var("AWS_ENDPOINT_URL_S3", endpoint);
            if endpoint.starts_with("http://") {
                std::env::set_var("AWS_ALLOW_HTTP", "true");
            }
        }
    }
    let region = match pin.region.as_deref() {
        Some(region) => region,
        None if pin.uri.starts_with("s3://") => DEFAULT_S3_PIN_REGION,
        None => return,
    };
    unsafe {
        std::env::set_var("AWS_REGION", region);
        std::env::set_var("AWS_DEFAULT_REGION", region);
    }
}

const DEFAULT_S3_PIN_REGION: &str = "us-west-2";

/// Apply local `@name` pin S3 backend keys before the multi-threaded Tokio runtime
/// starts. Same macOS `set_var` race as catalog serve: OpenDAL must see
/// `AWS_REGION` before worker threads exist.
pub(super) fn apply_local_pin_backend_env_before_runtime(
    reference: Option<&str>,
    settings_override: Option<&Path>,
) -> Result<()> {
    let Some(reference) = reference.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let Some(rest) = reference.strip_prefix('@') else {
        return Ok(());
    };
    let (name, _) = rest.split_once('/').unwrap_or((rest, ""));
    if name.is_empty() || RESERVED_PIN_NAMES.contains(&name) || name == DEFAULT_PIN_NAME {
        return Ok(());
    }
    validate_pin_name(name)?;
    let path = settings_path(settings_override)?;
    let settings = load_local_settings_or_default(&path)?;
    let Some(pin) = settings.pins.get(name) else {
        return Ok(());
    };
    if !pin.uri.starts_with("s3://") {
        return Ok(());
    }
    apply_pin_backend_env(pin);
    Ok(())
}

fn expand_catalog_pin(
    settings: &LocalSettings,
    name: &str,
    root: &str,
    suffix: &str,
    require_existing: bool,
) -> Result<String> {
    anyhow::ensure!(
        !suffix.is_empty(),
        "catalog pin '@{name}' requires a dataset, for example '@{name}/prod'"
    );
    let (dataset, path) = suffix.split_once('/').unwrap_or((suffix, ""));
    let path = normalize_pin_suffix(path);
    if !path.is_empty() {
        validate_pin_suffix(path)?;
    }
    let pin = settings.pins.get(name).ok_or_else(|| {
        cli_boundary_error(
            BoundaryCode::NotFound,
            format!("unknown Dataset pin '@{name}'"),
        )
    })?;
    let (access_key, secret_key) = pin.credentials_pair()?.ok_or_else(|| {
        cli_boundary_error(
            BoundaryCode::InvalidRequest,
            format!("catalog pin '@{name}' requires --ak and --sk"),
        )
    })?;
    let ticket = fetch_catalog_ticket(root, access_key, secret_key, dataset)?;
    crate::server::catalog::apply_library_env(&ticket);
    let expanded = join_pin_target(&ticket.uri, path)?;
    let location = DatasetLocation::parse(&expanded)?;
    if require_existing {
        if location.local_path().is_some_and(|path| !path.exists()) {
            return Err(cli_boundary_error(
                BoundaryCode::NotFound,
                format!(
                    "catalog dataset '{dataset}' is a local path that does not exist on this machine: {}",
                    location.as_str()
                ),
            ));
        }
        return Ok(location.into_existing()?.as_str().to_owned());
    }
    Ok(location.as_str().to_owned())
}

/// When `input` is a bare catalog pin (`@team` / `@team/`), return the pin
/// name and Directory URL. Dataset-qualified refs (`@team/prod`) return `None`.
pub(super) fn catalog_pin_directory_target(
    input: &str,
    settings_override: Option<&Path>,
) -> Result<Option<(String, String)>> {
    let input = input.trim();
    let Some(rest) = input.strip_prefix('@') else {
        return Ok(None);
    };
    let (name, suffix) = rest.split_once('/').unwrap_or((rest, ""));
    if !suffix.is_empty() || RESERVED_PIN_NAMES.contains(&name) || name == DEFAULT_PIN_NAME {
        return Ok(None);
    }
    validate_pin_name(name)?;
    let path = settings_path(settings_override)?;
    let settings = load_local_settings_or_default(&path)?;
    let root = settings
        .pins
        .get(name)
        .map(|pin| pin.uri.as_str())
        .ok_or_else(|| {
            cli_boundary_error(
                BoundaryCode::NotFound,
                format!("unknown Dataset pin '@{name}'"),
            )
        })?;
    if !root.starts_with("catalog://") {
        return Ok(None);
    }
    Ok(Some((name.to_owned(), root.to_owned())))
}

/// List libraries visible to a catalog pin's user credentials.
pub(super) fn list_catalog_pin_datasets(
    input: &str,
    settings_override: Option<&Path>,
) -> Result<Option<CatalogPinDatasetList>> {
    let Some((pin_name, catalog_url)) = catalog_pin_directory_target(input, settings_override)?
    else {
        return Ok(None);
    };
    let path = settings_path(settings_override)?;
    let settings = load_local_settings_or_default(&path)?;
    let pin = settings.pins.get(&pin_name).ok_or_else(|| {
        cli_boundary_error(
            BoundaryCode::NotFound,
            format!("unknown Dataset pin '@{pin_name}'"),
        )
    })?;
    let (access_key, secret_key) = pin.credentials_pair()?.ok_or_else(|| {
        cli_boundary_error(
            BoundaryCode::InvalidRequest,
            format!("catalog pin '@{pin_name}' requires --ak and --sk"),
        )
    })?;
    let datasets = fetch_catalog_datasets(&catalog_url, access_key, secret_key)?;
    Ok(Some(CatalogPinDatasetList {
        pin: format!("@{pin_name}"),
        catalog: catalog_url,
        datasets,
    }))
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct CatalogPinDatasetList {
    pub pin: String,
    pub catalog: String,
    pub datasets: Vec<crate::server::catalog::CatalogLibraryPublic>,
}

thread_local! {
    static CATALOG_TICKETS: std::cell::RefCell<
        HashMap<(String, String, String), crate::server::catalog::CatalogLibrary>,
    > = std::cell::RefCell::new(HashMap::new());
}

fn fetch_catalog_datasets(
    catalog_url: &str,
    access_key: &str,
    secret_key: &str,
) -> Result<Vec<crate::server::catalog::CatalogLibraryPublic>> {
    use crate::server::catalog::{ACCESS_KEY_HEADER, SECRET_KEY_HEADER, catalog_http_base};

    let base = catalog_http_base(catalog_url)?;
    let url = format!("{base}/api/v1/catalog/datasets");
    let response = reqwest::blocking::Client::new()
        .get(&url)
        .header(ACCESS_KEY_HEADER, access_key)
        .header(SECRET_KEY_HEADER, secret_key)
        .send()
        .with_context(|| format!("list catalog datasets at {catalog_url}"))?;
    let status = response.status();
    let body = response
        .text()
        .context("read catalog dataset list response")?;
    if !status.is_success() {
        return Err(cli_boundary_error(
            if status.as_u16() == 401 {
                BoundaryCode::InvalidRequest
            } else {
                BoundaryCode::Unavailable
            },
            format!("catalog dataset list failed ({status}): {body}"),
        ));
    }
    serde_json::from_str(&body).context("decode catalog dataset list")
}

fn fetch_catalog_ticket(
    catalog_url: &str,
    access_key: &str,
    secret_key: &str,
    dataset: &str,
) -> Result<crate::server::catalog::CatalogLibrary> {
    use crate::server::catalog::{ACCESS_KEY_HEADER, SECRET_KEY_HEADER, catalog_http_base};

    let cache_key = (
        catalog_url.to_owned(),
        access_key.to_owned(),
        dataset.to_owned(),
    );
    if let Some(ticket) = CATALOG_TICKETS.with(|tickets| tickets.borrow().get(&cache_key).cloned())
    {
        return Ok(ticket);
    }
    let base = catalog_http_base(catalog_url)?;
    let url = format!(
        "{base}/api/v1/catalog/datasets/{}",
        urlencoding_dataset(dataset)
    );
    let response = reqwest::blocking::Client::new()
        .get(&url)
        .header(ACCESS_KEY_HEADER, access_key)
        .header(SECRET_KEY_HEADER, secret_key)
        .send()
        .with_context(|| format!("request catalog dataset '{dataset}'"))?;
    let status = response.status();
    let body = response.text().context("read catalog dataset response")?;
    if !status.is_success() {
        return Err(cli_boundary_error(
            if status.as_u16() == 404 {
                BoundaryCode::NotFound
            } else if status.as_u16() == 401 {
                BoundaryCode::InvalidRequest
            } else {
                BoundaryCode::Unavailable
            },
            format!("catalog dataset '{dataset}' failed: HTTP {status} {body}"),
        ));
    }
    let ticket: crate::server::catalog::CatalogLibrary =
        serde_json::from_str(&body).context("decode catalog dataset ticket")?;
    CATALOG_TICKETS.with(|tickets| {
        tickets.borrow_mut().insert(cache_key, ticket.clone());
    });
    Ok(ticket)
}

fn urlencoding_dataset(dataset: &str) -> String {
    dataset
        .bytes()
        .flat_map(|byte| {
            if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' {
                vec![byte as char]
            } else {
                format!("%{byte:02X}").chars().collect()
            }
        })
        .collect()
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
        // Directory-style refs often end with `/` (e.g. `@origin/foo/`); treat
        // that as equivalent to the same path without trailing separators.
        let suffix = normalize_pin_suffix(suffix);
        validate_pin_suffix(suffix)?;
        if name == DEFAULT_PIN_NAME {
            let root = resolve_default_pin(settings_override)?;
            join_pin_target(&root, suffix)?
        } else if RESERVED_PIN_NAMES.contains(&name) {
            expand_builtin_pin(input)?
        } else {
            validate_pin_name(name)?;
            let path = settings_path(settings_override)?;
            let settings = load_local_settings_or_default(&path)?;
            let pin = settings.pins.get(name).ok_or_else(|| {
                cli_boundary_error(
                    BoundaryCode::NotFound,
                    format!("unknown Dataset pin '@{name}'"),
                )
            })?;
            if pin.uri.starts_with("catalog://") {
                expand_catalog_pin(&settings, name, &pin.uri, suffix, require_existing)?
            } else {
                let expanded = join_pin_target(&pin.uri, suffix)?;
                apply_pin_backend_env(pin);
                expanded
            }
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

fn normalize_pin_suffix(suffix: &str) -> &str {
    suffix.trim_matches('/')
}

fn validate_pin_suffix(suffix: &str) -> Result<()> {
    let suffix = normalize_pin_suffix(suffix);
    if suffix.is_empty() {
        return Ok(());
    }
    anyhow::ensure!(
        !suffix.contains(['\\', '\0']),
        "pin suffix contains an invalid character"
    );
    for component in suffix.split('/') {
        anyhow::ensure!(
            !component.is_empty() && component != "." && component != "..",
            "pin suffix must not contain empty, '.', or '..' segments"
        );
    }
    Ok(())
}

fn join_pin_target(root: &str, suffix: &str) -> Result<String> {
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
        None => resolve_default_pin(settings_override),
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
    let warehouse = resolve_default_pin(settings_override)?;
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

#[cfg(test)]
mod pin_config_parse_tests {
    use super::*;

    #[test]
    fn parses_nested_pins_tables() {
        let content = r#"
[pins.rfs]
uri = "s3://test/test"
endpoint = "http://127.0.0.1:9000"
access_key = "123"
secret_key = "123"

[pins.testcata]
uri = "catalog://127.0.0.1:6001"
access_key = "ak"
secret_key = "sk"
"#;
        let settings: LocalSettings = toml::from_str(content).expect("parse");
        assert_eq!(settings.pins["rfs"].uri, "s3://test/test");
        assert_eq!(
            settings.pins["rfs"].endpoint.as_deref(),
            Some("http://127.0.0.1:9000")
        );
        assert_eq!(settings.pins["testcata"].uri, "catalog://127.0.0.1:6001");
    }

    #[test]
    fn pin_suffix_allows_trailing_and_leading_slashes() {
        assert!(validate_pin_suffix("SweEval/guoxu1/").is_ok());
        assert!(validate_pin_suffix("/SweEval/guoxu1///").is_ok());
        assert!(validate_pin_suffix("/").is_ok());
        assert!(validate_pin_suffix("").is_ok());
    }

    #[test]
    fn pin_suffix_still_rejects_dot_and_empty_middle_segments() {
        assert!(validate_pin_suffix("a/../b").is_err());
        assert!(validate_pin_suffix("a/./b").is_err());
        assert!(validate_pin_suffix("a//b").is_err());
    }

    #[test]
    fn expand_dataset_reference_trims_trailing_slash() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let config = temporary.path().join("config.toml");
        std::fs::write(
            &config,
            r#"
[pins.origin]
uri = "s3://example-bucket/root"
"#,
        )
        .expect("write config");
        let expanded =
            expand_dataset_reference("@origin/SweEval/guoxu1/", Some(&config), false)
                .expect("expand");
        assert_eq!(expanded, "s3://example-bucket/root/SweEval/guoxu1");
    }
}
