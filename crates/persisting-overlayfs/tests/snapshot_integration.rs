use persisting_overlayfs::{discard_redb_upper, redb_upper_status};

#[test]
fn missing_snapshot_status_is_empty_and_side_effect_free() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("missing.redb");

    let status = redb_upper_status(&database).unwrap();

    assert_eq!(status.changed_paths, 0);
    assert_eq!(status.whiteouts, 0);
    assert_eq!(status.opaque_directories, 0);
    assert_eq!(status.generation, 0);
    assert!(status.sample_paths.is_empty());
    assert!(!database.exists());
    discard_redb_upper(&database).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires an enabled macFUSE kernel extension"]
fn snapshot_public_api_mounts_persists_and_applies_without_upper_directory() {
    use persisting_overlayfs::{apply_redb_upper, mount, OverlayMountConfig};
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    let stage = temp.path().join("stage");
    let database = stage.join("snapshot.redb");
    let merged = stage.join("merged");
    fs::create_dir_all(&lower).unwrap();
    fs::write(lower.join("base"), b"lower").unwrap();
    fs::write(lower.join("deleted"), b"gone").unwrap();

    let session = mount(OverlayMountConfig::new_redb(
        vec![lower.clone()],
        database.clone(),
        merged.clone(),
    ))
    .unwrap();
    let base = merged.join("base");
    let initial = (0..250)
        .find_map(|_| match fs::read(&base) {
            Ok(data) => Some(data),
            Err(_) => {
                std::thread::sleep(Duration::from_millis(20));
                None
            }
        })
        .expect("snapshot mount did not become readable within five seconds");
    assert_eq!(initial, b"lower");
    fs::write(merged.join("base"), b"copied-up").unwrap();
    fs::remove_file(merged.join("deleted")).unwrap();
    fs::create_dir(merged.join("directory")).unwrap();
    fs::write(merged.join("directory/before"), b"nested").unwrap();
    fs::rename(
        merged.join("directory/before"),
        merged.join("directory/after"),
    )
    .unwrap();
    fs::write(merged.join("created"), b"new").unwrap();
    fs::hard_link(merged.join("created"), merged.join("created-link")).unwrap();
    std::os::unix::fs::symlink("created", merged.join("created-symlink")).unwrap();

    assert!(database.is_file());
    assert!(
        !stage.join("upper").exists(),
        "snapshot backend must not materialize a parallel upper directory"
    );
    session.unmount().unwrap();

    let status = redb_upper_status(&database).unwrap();
    assert!(status.changed_paths >= 6);
    assert_eq!(status.whiteouts, 1);
    assert!(status.generation > 0);

    apply_redb_upper(&database, &lower).unwrap();
    assert_eq!(fs::read(lower.join("base")).unwrap(), b"copied-up");
    assert!(!lower.join("deleted").exists());
    assert_eq!(fs::read(lower.join("directory/after")).unwrap(), b"nested");
    assert_eq!(
        fs::read_link(lower.join("created-symlink")).unwrap(),
        std::path::PathBuf::from("created")
    );
    assert_eq!(
        fs::metadata(lower.join("created")).unwrap().ino(),
        fs::metadata(lower.join("created-link")).unwrap().ino()
    );

    discard_redb_upper(&database).unwrap();
    assert!(!database.exists());
}
