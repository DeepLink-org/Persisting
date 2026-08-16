use super::*;

const MAX_WAREHOUSE_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_WAREHOUSE_DATASETS: usize = 128;
const SETTINGS_ENV: &str = "PCHRONICLE_SETTINGS";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LocalSettings {
    default_warehouse: String,
}

pub(super) fn default_settings_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(SETTINGS_ENV).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.map(|base| base.join("pchronicle/settings.toml"))
        .context("cannot locate the user configuration directory; pass --settings <FILE>")
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

pub(super) fn resolve_default_warehouse(settings_override: Option<&Path>) -> Result<String> {
    let path = settings_path(settings_override)?;
    anyhow::ensure!(
        path.exists(),
        "default Warehouse is not configured; run `pchronicle default <DIRECTORY>` (settings: {})",
        path.display()
    );
    let settings = load_local_settings(&path)?;
    let warehouse = normalize_and_validate_dataset_uri(&settings.default_warehouse)
        .context("validate configured default Warehouse")?;
    anyhow::ensure!(
        !warehouse.contains("://") && Path::new(&warehouse).is_dir(),
        "configured default Warehouse must be a local directory"
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
    let warehouse = if let Some(directory) = args.directory {
        if !directory.exists() {
            std::fs::create_dir_all(&directory).with_context(|| {
                format!("create default Warehouse directory {}", directory.display())
            })?;
        }
        anyhow::ensure!(directory.is_dir(), "default Warehouse must be a directory");
        let warehouse = std::fs::canonicalize(&directory)
            .context("canonicalize default Warehouse directory")?
            .to_string_lossy()
            .into_owned();
        write_local_settings(
            &path,
            &LocalSettings {
                default_warehouse: warehouse.clone(),
            },
        )?;
        writeln!(stderr, "settings={} updated=true", path.display())
            .context("write pChronicle default metadata")?;
        warehouse
    } else {
        resolve_default_warehouse(settings_override)?
    };
    writeln!(stdout, "{warehouse}").context("write default Warehouse")?;
    Ok(())
}

pub(super) fn resolve_dataset_uri(
    explicit: Option<&str>,
    settings_override: Option<&Path>,
) -> Result<String> {
    match explicit {
        Some(uri) => normalize_and_validate_dataset_uri(uri),
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

pub(super) fn load_warehouse_config(path: &Path) -> Result<server::ChronicleServerConfig> {
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
        let input = if !dataset.uri.contains("://") && Path::new(&dataset.uri).is_relative() {
            config_dir.join(&dataset.uri).to_string_lossy().into_owned()
        } else {
            dataset.uri
        };
        let uri = normalize_and_validate_dataset_uri(&input)
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
