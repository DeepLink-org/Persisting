//! Render markdown in the terminal (termimad).

use std::io::{self, IsTerminal, Write};

use termimad::MadSkin;

fn skin() -> MadSkin {
    if std::env::var_os("NO_COLOR").is_some() {
        MadSkin::no_style()
    } else {
        MadSkin::default()
    }
}

pub fn stdout_is_tty() -> bool {
    io::stdout().is_terminal()
}

pub fn stdout_supports_markdown() -> bool {
    std::env::var_os("NO_COLOR").is_none() && stdout_is_tty()
}

pub fn stderr_supports_markdown() -> bool {
    std::env::var_os("NO_COLOR").is_none() && io::stderr().is_terminal()
}

/// TTY + termimad when possible; otherwise raw markdown bytes on stdout.
pub fn print_markdown_stdout(title: Option<&str>, markdown: &str) {
    if stdout_supports_markdown() {
        print_markdown_rendered_stdout(title, markdown);
    } else {
        print_markdown_raw_stdout(title, markdown);
    }
}

fn print_markdown_rendered_stdout(title: Option<&str>, markdown: &str) {
    let skin = skin();
    let mut out = io::stdout().lock();
    if let Some(title) = title {
        let header = format!("## {title}\n\n");
        if skin.write_text_on(&mut out, &header).is_err() {
            let _ = writeln!(out, "{header}");
        }
    }
    if !markdown.is_empty() {
        if skin.write_text_on(&mut out, markdown).is_err() {
            let _ = write!(out, "{markdown}");
        }
    }
    let _ = writeln!(out);
    let _ = out.flush();
}

fn print_markdown_raw_stdout(title: Option<&str>, markdown: &str) {
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
    if !stderr_supports_markdown() {
        eprintln!("\n{title}\n\n{markdown}\n{}", "-".repeat(72));
        return;
    }
    let skin = skin();
    let mut err = io::stderr().lock();
    let _ = writeln!(err);
    let header = format!("## {title}\n\n");
    if skin.write_text_on(&mut err, &header).is_err() {
        let _ = writeln!(err, "{header}");
    }
    if skin.write_text_on(&mut err, markdown).is_err() {
        let _ = writeln!(err, "{markdown}");
    }
    let _ = writeln!(err);
    let _ = writeln!(err, "{}", "-".repeat(72));
    let _ = err.flush();
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
