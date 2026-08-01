use std::process::Command;

#[cfg(not(feature = "dlcapt"))]
#[test]
fn dlcapt_backend_without_feature_explains_how_to_enable_it() {
    let output = Command::new(env!("CARGO_BIN_EXE_persisting"))
        .args([
            "gateway",
            "serve",
            "--backend",
            "dlcapt",
            "-c",
            "proxy.toml",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("rebuild persisting-cli with --features dlcapt")
    );
}

#[cfg(feature = "dlcapt")]
#[test]
fn dlcapt_backend_rejects_capture_only_output_directory() {
    let output = Command::new(env!("CARGO_BIN_EXE_persisting"))
        .args([
            "gateway",
            "serve",
            "--backend",
            "dlcapt",
            "-c",
            "proxy.toml",
            "-o",
            "store",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("-o is only supported by the capture backend")
    );
}
