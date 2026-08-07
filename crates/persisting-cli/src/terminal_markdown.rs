//! Write Markdown output consistently to terminals, pipes, and files.

use std::io::{self, IsTerminal, Write};

pub fn stdout_is_tty() -> bool {
    io::stdout().is_terminal()
}

/// Write raw Markdown bytes on stdout regardless of whether it is a TTY.
///
/// Keeping the output identical across terminals and pipes makes it predictable
/// for users and avoids coupling the CLI to a terminal-specific renderer.
pub fn print_markdown_stdout(title: Option<&str>, markdown: &str) {
    let mut out = io::stdout().lock();
    if let Some(title) = title {
        let _ = writeln!(out, "## {title}\n");
    }
    if !markdown.is_empty() {
        let _ = write!(out, "{markdown}");
    }
    let _ = writeln!(out);
    let _ = out.flush();
}

/// Print a titled markdown block to stderr (manual judge prompts).
pub fn print_section(title: &str, markdown: &str) {
    eprintln!("\n{title}\n\n{markdown}\n{}", "-".repeat(72));
}

pub fn format_turn_markdown(
    turn_idx: usize,
    turn_total: usize,
    call_id: &str,
    user: &str,
    assistant: &str,
) -> String {
    format!(
        "### Turn {turn_idx}/{turn_total} (`{call_id}`)\n\n\
         **User**\n\n\
         {user}\n\n\
         ---\n\n\
         **Assistant**\n\n\
         {assistant}\n"
    )
}
