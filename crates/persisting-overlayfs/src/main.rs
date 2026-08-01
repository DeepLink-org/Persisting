//! Cross-platform FUSE overlay for pVisor.
//!
//! Portable fuse-overlayfs-compatible semantics:
//! - Reads resolve upper → lowers (top to bottom)
//! - Writes copy-up into `upper`
//! - Deletes create `.wh.<name>` when the name exists in a lower
//! - Opaque dirs use `.wh..wh..opq`
//!
//! Compatible with pVisor's `apply_overlay` whiteout handling.

use anyhow::{bail, Context, Result};
use clap::Parser;
use persisting_overlayfs::{run_foreground, OverlayMountConfig};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "persisting-overlayfs",
    about = "Cross-platform FUSE overlay for pVisor (macFUSE / libfuse)"
)]
struct Args {
    /// Mount options: lowerdir=a:b plus either upperdir=u[,workdir=w] or database=db.
    #[arg(short = 'o', long = "options", value_name = "OPTS")]
    #[arg(required = true)]
    options: Vec<String>,
    /// Mount point (merged view).
    #[arg(value_name = "MOUNTPOINT")]
    mountpoint: PathBuf,
    /// Debug logging.
    #[arg(short = 'd', long = "debug", default_value_t = false)]
    debug: bool,
}

#[derive(Debug)]
struct MountOpts {
    lowerdir: Vec<PathBuf>,
    upperdir: Option<PathBuf>,
    database: Option<PathBuf>,
    workdir: Option<PathBuf>,
    allow_other: bool,
    allow_root: bool,
    default_permissions: bool,
    read_only: bool,
    fsname: String,
    backend: Option<String>,
}

fn split_escaped(raw: &str, separator: char) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut characters = raw.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\' {
            match characters.peek().copied() {
                Some(next) if next == separator || next == '\\' => {
                    let _ = characters.next();
                    current.push(next);
                }
                _ => current.push(character),
            }
        } else if character == separator {
            values.push(std::mem::take(&mut current));
        } else {
            current.push(character);
        }
    }
    values.push(current);
    values
}

fn parse_options(raw: &str) -> Result<MountOpts> {
    let mut lowerdir = None;
    let mut upperdir = None;
    let mut database = None;
    let mut workdir = None;
    let mut allow_other = false;
    let mut allow_root = false;
    let mut default_permissions = true;
    let mut read_only = false;
    let mut fsname = "persisting-overlayfs".to_string();
    let mut backend = None;
    for part in split_escaped(raw, ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((k, v)) = part.split_once('=') else {
            match part {
                "allow_other" => allow_other = true,
                "allow_root" => allow_root = true,
                "default_permissions" => default_permissions = true,
                "nodefault_permissions" => default_permissions = false,
                "ro" => read_only = true,
                "rw" => read_only = false,
                _ => log::debug!("ignoring unsupported mount option: {part}"),
            }
            continue;
        };
        match k {
            "lowerdir" => {
                lowerdir = Some(
                    split_escaped(v, ':')
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                        .collect::<Vec<_>>(),
                );
            }
            "upperdir" => upperdir = Some(PathBuf::from(v)),
            "database" => database = Some(PathBuf::from(v)),
            "workdir" => workdir = Some(PathBuf::from(v)),
            "fsname" => fsname = v.to_string(),
            "backend" if matches!(v, "kernel" | "fskit") => backend = Some(v.to_string()),
            "backend" => bail!("unsupported macFUSE backend: {v}"),
            _ => log::debug!("ignoring unsupported mount option: {part}"),
        }
    }
    let lowerdir = lowerdir.context("missing lowerdir=")?;
    if lowerdir.is_empty() {
        bail!("lowerdir must list at least one path");
    }
    match (&upperdir, &database) {
        (None, None) => bail!("missing upper backend: specify upperdir= or database="),
        (Some(_), Some(_)) => bail!("upperdir= and database= are mutually exclusive"),
        (None, Some(_)) if workdir.is_some() => {
            bail!("workdir= is only valid with the directory upper backend")
        }
        _ => {}
    }
    Ok(MountOpts {
        lowerdir,
        upperdir,
        database,
        workdir,
        allow_other,
        allow_root,
        default_permissions,
        read_only,
        fsname,
        backend,
    })
}

fn main() -> Result<()> {
    let args = Args::parse();
    let level = if args.debug {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Warn
    };
    env_logger::Builder::new()
        .filter_level(level)
        .parse_default_env()
        .format_timestamp(None)
        .init();

    let opts = parse_options(&args.options.join(","))?;
    let mut config = match (opts.upperdir, opts.database) {
        (Some(upperdir), None) => {
            OverlayMountConfig::new(opts.lowerdir, upperdir, opts.workdir, args.mountpoint)
        }
        (None, Some(database)) => {
            OverlayMountConfig::new_redb(opts.lowerdir, database, args.mountpoint)
        }
        _ => unreachable!("parse_options validates the upper backend"),
    };
    config.allow_other = opts.allow_other;
    config.allow_root = opts.allow_root;
    config.default_permissions = opts.default_permissions;
    config.read_only = opts.read_only;
    config.fsname = opts.fsname;
    config.backend = opts.backend;
    config.debug = args.debug;
    run_foreground(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_accept_escaped_layer_separators() {
        let options =
            parse_options(r"lowerdir=/base:/path\:with\:colon,upperdir=/u,workdir=/w,allow_other")
                .expect("options");
        assert_eq!(
            options.lowerdir,
            vec![PathBuf::from("/base"), PathBuf::from("/path:with:colon")]
        );
        assert!(options.allow_other);
    }

    #[test]
    fn cli_accepts_repeated_option_arguments() {
        let args = Args::try_parse_from([
            "persisting-overlayfs",
            "-o",
            "lowerdir=/lower,upperdir=/upper",
            "-o",
            "workdir=/work",
            "/merged",
        ])
        .expect("cli");
        let options = parse_options(&args.options.join(",")).expect("options");
        assert_eq!(options.workdir, Some(PathBuf::from("/work")));
        assert_eq!(args.mountpoint, PathBuf::from("/merged"));
    }

    #[test]
    fn database_upper_is_exclusive() {
        let options =
            parse_options("lowerdir=/lower,database=/stage/upper.redb").expect("database options");
        assert_eq!(options.database, Some(PathBuf::from("/stage/upper.redb")));
        assert!(options.upperdir.is_none());
        assert!(
            parse_options("lowerdir=/lower,database=/db,upperdir=/upper").is_err(),
            "two upper backends must be rejected"
        );
        assert!(
            parse_options("lowerdir=/lower,database=/db,workdir=/work").is_err(),
            "redb has no work directory"
        );
    }
}
