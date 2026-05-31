//! Startup banner: how to point agents at a running capture proxy.

use std::path::Path;

use persisting_capture::injection::capture_openai_v1_base;

use super::CaptureFormat;

pub struct ServeBanner<'a> {
    pub listen: &'a str,
    pub admin_listen: &'a str,
    pub output_dir: &'a Path,
    pub agent_id: &'a str,
    pub format: CaptureFormat,
    /// `traj proxy start` (background) vs `traj proxy` (foreground).
    pub background: bool,
    pub pid: Option<u32>,
}

pub fn eprint_serve_banner(info: &ServeBanner<'_>) {
    let proxy = http_base(info.listen);
    let admin = http_base(info.admin_listen);
    let openai_v1 = capture_openai_v1_base(info.listen);
    let store = info.output_dir.display();

    eprintln!();
    if info.background {
        eprintln!(
            "[persisting-cli] traj proxy started: pid={} proxy={proxy} admin={admin}",
            info.pid.unwrap_or(0)
        );
    } else {
        eprintln!("[persisting-cli] traj proxy: proxy={proxy} admin={admin}");
    }
    eprintln!(
        "[persisting-cli] traj: store={store} agent_id={} format={}",
        info.agent_id,
        info.format.as_str()
    );
    eprintln!();
    eprintln!("Point your agent at the proxy (new terminal):");
    eprintln!();
    eprintln!("  export HTTP_PROXY={proxy} HTTPS_PROXY={proxy}");
    eprintln!("  export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost");
    eprintln!("  export OPENAI_BASE_URL={openai_v1}");
    eprintln!("  export ANTHROPIC_BASE_URL={proxy}");
    eprintln!("  claude");
    eprintln!();
    eprintln!("  # Codex (ignores OPENAI_BASE_URL):");
    eprintln!("  codex -c 'openai_base_url=\"{openai_v1}\"'");
    eprintln!();
    eprintln!("One-shot in-process (stop proxy on same -o first):");
    eprintln!(
        "  persisting traj capture -o {store} -c <proxy.toml> -f {} -- claude",
        info.format.as_str()
    );
    eprintln!();
    eprintln!("Inspect:");
    eprintln!("  persisting traj proxy list -o {store}");
    eprintln!("  persisting traj proxy status -o {store}");
    eprintln!("  persisting traj stats {store} --detail");
    if info.background {
        eprintln!("  persisting traj proxy stop -o {store}");
    } else {
        eprintln!("  Ctrl+C to stop this server");
    }
    eprintln!();
}

fn http_base(listen: &str) -> String {
    if listen.starts_with("http://") || listen.starts_with("https://") {
        listen.trim_end_matches('/').to_string()
    } else {
        format!("http://{}", listen.trim_end_matches('/'))
    }
}
