//! `traj stats` output backends.
//!
//! | Backend | Role |
//! |---------|------|
//! | **plain** | Stable plain text; works everywhere (logs, pipes, CI). |
//! | **md** | Markdown for reading; TTY uses termimad, pipe emits raw `.md`. |
//! | **toml** / **json** | Structured output for scripts and other tools. |
//! | **auto** | TTY → md, pipe → toml. |

use anyhow::Result;
use clap::ValueEnum;
use persisting_proto::{SessionJudgeStats, TrajectoryJudgeStatsResponse, TrajectoryStatsResponse};

use crate::terminal_markdown::print_markdown_stdout;
use crate::trajectory_detail::{print_trajectory_stats_detail_plain, DetailNode};
use crate::trajectory_stdout_toml::{
    print_trajectory_stats_as_json, print_trajectory_stats_as_toml,
    print_trajectory_stats_list_as_json, print_trajectory_stats_list_as_toml,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum StatsOutputBackend {
    /// TTY → md, pipe → toml.
    #[default]
    Auto,
    /// Plain text (stable, no markup).
    Plain,
    /// Markdown (human-readable).
    Md,
    /// TOML (machine-readable).
    Toml,
    /// JSON (machine-readable).
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedStatsOutputBackend {
    Plain,
    Md,
    Toml,
    Json,
}

impl StatsOutputBackend {
    pub fn resolve(self) -> ResolvedStatsOutputBackend {
        match self {
            StatsOutputBackend::Auto => {
                if crate::terminal_markdown::stdout_is_tty() {
                    ResolvedStatsOutputBackend::Md
                } else {
                    ResolvedStatsOutputBackend::Toml
                }
            }
            StatsOutputBackend::Plain => ResolvedStatsOutputBackend::Plain,
            StatsOutputBackend::Md => ResolvedStatsOutputBackend::Md,
            StatsOutputBackend::Toml => ResolvedStatsOutputBackend::Toml,
            StatsOutputBackend::Json => ResolvedStatsOutputBackend::Json,
        }
    }
}

pub fn supports_detail_tree(backend: ResolvedStatsOutputBackend) -> bool {
    matches!(
        backend,
        ResolvedStatsOutputBackend::Plain | ResolvedStatsOutputBackend::Md
    )
}

pub fn print_trajectory_stats_summary(
    resp: &TrajectoryStatsResponse,
    backend: ResolvedStatsOutputBackend,
) -> Result<()> {
    match backend {
        ResolvedStatsOutputBackend::Plain => {
            print_stats_summary_plain(resp);
            Ok(())
        }
        ResolvedStatsOutputBackend::Md => {
            let title = format!("{} / {}", resp.agent_id, resp.session_id);
            print_markdown_stdout(Some(&title), &format_stats_markdown(resp));
            Ok(())
        }
        ResolvedStatsOutputBackend::Toml => print_trajectory_stats_as_toml(resp),
        ResolvedStatsOutputBackend::Json => print_trajectory_stats_as_json(resp),
    }
}

pub fn print_trajectory_stats_list(
    storage: &str,
    sessions: &[TrajectoryStatsResponse],
    judge: Option<&TrajectoryJudgeStatsResponse>,
    backend: ResolvedStatsOutputBackend,
) -> Result<()> {
    match backend {
        ResolvedStatsOutputBackend::Plain => {
            print_stats_list_plain(storage, sessions, judge);
            Ok(())
        }
        ResolvedStatsOutputBackend::Md => {
            print_markdown_stdout(
                Some(&format!("Trajectory stats · {storage}")),
                &format_stats_list_markdown(sessions, judge),
            );
            Ok(())
        }
        ResolvedStatsOutputBackend::Toml => {
            print_trajectory_stats_list_as_toml(storage, sessions, judge)
        }
        ResolvedStatsOutputBackend::Json => {
            print_trajectory_stats_list_as_json(storage, sessions, judge)
        }
    }
}

pub fn print_trajectory_stats_detail(
    stats: &TrajectoryStatsResponse,
    root: &DetailNode,
    agent_id: Option<&str>,
    backend: ResolvedStatsOutputBackend,
) -> Result<()> {
    match backend {
        ResolvedStatsOutputBackend::Plain => {
            print_trajectory_stats_detail_plain(stats, root, agent_id)
        }
        ResolvedStatsOutputBackend::Md => print_trajectory_stats_detail_md(stats, root, agent_id),
        ResolvedStatsOutputBackend::Toml | ResolvedStatsOutputBackend::Json => Ok(()),
    }
}

pub fn print_stats_section_divider(label: &str, backend: ResolvedStatsOutputBackend) {
    match backend {
        ResolvedStatsOutputBackend::Plain => println!("--- {label} ---"),
        ResolvedStatsOutputBackend::Md => print_markdown_stdout(Some(label), ""),
        ResolvedStatsOutputBackend::Toml | ResolvedStatsOutputBackend::Json => {
            println!();
        }
    }
}

fn print_stats_summary_plain(resp: &TrajectoryStatsResponse) {
    println!("agent_id: {}", resp.agent_id);
    println!("session_id: {}", resp.session_id);
    println!("row_count: {}", resp.row_count);
    println!("dataset: {}", resp.dataset);
    println!("status: {}", resp.status);
    if let Some(j) = &resp.judge {
        if j.judgment_count > 0 {
            print_judge_plain(j);
        }
    }
    if !resp.note.is_empty() {
        println!("note: {}", resp.note);
    }
}

fn print_stats_list_plain(
    storage: &str,
    sessions: &[TrajectoryStatsResponse],
    judge: Option<&TrajectoryJudgeStatsResponse>,
) {
    println!("storage: {storage}");
    println!("session_count: {}", sessions.len());
    if let Some(j) = judge {
        if j.judgment_count > 0 {
            println!("judged_session_count: {}", j.judged_session_count);
            println!("judgment_count: {}", j.judgment_count);
            println!("rubric_count: {}", j.rubric_count);
        }
    }
    for (i, s) in sessions.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("--- session {} ---", i + 1);
        print_stats_summary_plain(s);
    }
}

fn print_judge_plain(j: &SessionJudgeStats) {
    println!("judgment_count: {}", j.judgment_count);
    println!("turn_judgments: {}", j.turn_judgments);
    println!("story_judgments: {}", j.story_judgments);
    if let Some(avg) = j.avg_score {
        println!("avg_score: {avg:.1}");
    }
    println!(
        "verdict_pass: {}  verdict_partial: {}  verdict_fail: {}",
        j.verdict_pass, j.verdict_partial, j.verdict_fail
    );
    if !j.rubric_ids.is_empty() {
        println!("rubric_ids: {}", j.rubric_ids.join(", "));
    }
    if j.manual_count > 0 {
        println!("manual_count: {}", j.manual_count);
    }
    if !j.layers_path.is_empty() {
        println!("layers_path: {}", j.layers_path);
    }
}

fn print_trajectory_stats_detail_md(
    stats: &TrajectoryStatsResponse,
    root: &DetailNode,
    agent_id: Option<&str>,
) -> Result<()> {
    if stats.status != "ok" {
        print_markdown_stdout(
            Some(&stats.session_id),
            &format!(
                "status: **{}** · row_count: {}",
                stats.status, stats.row_count
            ),
        );
        return Ok(());
    }

    let agg = crate::trajectory_detail::aggregate_summary_for_display(root);
    let subagents = crate::trajectory_detail::count_unique_subagents_for_display(root);
    let models = if agg.models.is_empty() {
        "-".to_string()
    } else {
        agg.models.join(", ")
    };
    let turn_line = if subagents > 0 {
        format!("**{} turns** ({subagents} subagents)", agg.turn_count)
    } else {
        format!("**{} turns**", agg.turn_count)
    };
    let session_label = match agent_id {
        Some(agent) if agent != stats.session_id => format!("{agent} / {}", stats.session_id),
        _ => stats.session_id.clone(),
    };
    print_markdown_stdout(
        Some(&format!("Session {session_label}")),
        &format_stats_detail_markdown(root, &agg, &models, &turn_line, stats),
    );
    Ok(())
}

fn format_stats_markdown(resp: &TrajectoryStatsResponse) -> String {
    let mut md = format!(
        "| Field | Value |\n| --- | --- |\n\
         | Events | {} |\n\
         | Dataset | `{}` |\n\
         | Status | {} |\n",
        resp.row_count, resp.dataset, resp.status,
    );
    if let Some(j) = &resp.judge {
        if j.judgment_count > 0 {
            md.push_str("\n### Judge\n\n");
            md.push_str(&format_judge_block(j));
        }
    }
    if !resp.note.is_empty() {
        md.push_str(&format!("\n_{}_", resp.note));
    }
    md
}

fn format_stats_list_markdown(
    sessions: &[TrajectoryStatsResponse],
    judge: Option<&TrajectoryJudgeStatsResponse>,
) -> String {
    let mut md = format!("**{} session(s)**\n\n", sessions.len());
    if let Some(j) = judge {
        if j.judgment_count > 0 {
            md.push_str(&format!(
                "- Judged: **{}/{}** sessions · **{}** judgment(s) · **{}** rubric(s)\n\n",
                j.judged_session_count, j.session_count, j.judgment_count, j.rubric_count,
            ));
        }
    }
    md.push_str("| Session | Events | Judge | Avg score |\n| --- | ---: | ---: | ---: |\n");
    for s in sessions {
        let judge_n = s.judge.as_ref().map(|j| j.judgment_count).unwrap_or(0);
        let avg = s
            .judge
            .as_ref()
            .and_then(|j| j.avg_score)
            .map(|v| format!("{v:.1}"))
            .unwrap_or_else(|| "-".into());
        let sid = truncate_id(&s.session_id, 36);
        md.push_str(&format!(
            "| `{sid}` | {} | {judge_n} | {avg} |\n",
            s.row_count
        ));
    }
    if let Some(j) = judge {
        if !j.rubrics.is_empty() {
            md.push_str("\n### Rubrics\n\n");
            md.push_str("| Rubric | Judgments | Avg | Pass |\n| --- | ---: | ---: | ---: |\n");
            for r in &j.rubrics {
                md.push_str(&format!(
                    "| `{}` | {} | {:.1} | {} |\n",
                    r.rubric_id, r.judgment_count, r.avg_score, r.verdict_pass,
                ));
            }
        }
    }
    md
}

fn format_stats_detail_markdown(
    root: &DetailNode,
    agg: &crate::trajectory_detail::SessionDetailSummary,
    models: &str,
    turn_line: &str,
    stats: &TrajectoryStatsResponse,
) -> String {
    let mut md = format!(
        "{turn_line} · **{} blocks** · prompt **{}** tok · completion **{}** tok · models: {models}\n\n",
        agg.block_count, agg.total_prompt_tokens, agg.total_completion_tokens,
    );
    if let Some(j) = &stats.judge {
        if j.judgment_count > 0 {
            md.push_str(&format!("Judge: **{}** judgment(s)", j.judgment_count));
            if let Some(avg) = j.avg_score {
                md.push_str(&format!(", avg **{avg:.1}**"));
            }
            md.push_str("\n\n");
        }
    }
    md.push_str("### Turn tree\n\n");
    md.push_str("| Turn | User | Model | TTFT | TPOT | Out tok |\n| ---: | ---: | ---: | ---: | ---: | ---: |\n");
    append_detail_table_rows(&mut md, root, 0);
    md
}

fn append_detail_table_rows(md: &mut String, node: &DetailNode, depth: usize) {
    let prefix = if depth > 0 {
        format!("└─ {} ", node.label)
    } else {
        String::new()
    };
    for turn in &node.turns {
        let turn_label = if prefix.is_empty() {
            turn.turn.index.to_string()
        } else {
            format!("{prefix}{}", turn.turn.index)
        };
        let out_tok = turn
            .turn
            .completion_tokens
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into());
        md.push_str(&format!(
            "| {} | {}c | {}c | {} | {} | {} |\n",
            turn_label,
            turn.turn.user_chars,
            turn.turn.assistant_chars,
            crate::trajectory_detail::fmt_ms_display(turn.turn.ttft_ms),
            crate::trajectory_detail::fmt_tpot_display(turn.turn.tpot_ms),
            out_tok,
        ));
        for sub in &turn.subagents {
            append_detail_table_rows(md, sub, depth + 1);
        }
    }
}

fn format_judge_block(j: &SessionJudgeStats) -> String {
    let mut md = format!(
        "- Judgments: **{}** (turn {}, story {})\n",
        j.judgment_count, j.turn_judgments, j.story_judgments,
    );
    if let Some(avg) = j.avg_score {
        md.push_str(&format!("- **Avg score:** {avg:.1}\n"));
    }
    md.push_str(&format!(
        "- Verdicts: pass **{}** · partial **{}** · fail **{}\n",
        j.verdict_pass, j.verdict_partial, j.verdict_fail,
    ));
    if !j.rubric_ids.is_empty() {
        md.push_str(&format!("- Rubrics: `{}`\n", j.rubric_ids.join("`, `")));
    }
    if j.manual_count > 0 {
        md.push_str(&format!("- Manual: **{}**\n", j.manual_count));
    }
    if !j.layers_path.is_empty() {
        md.push_str(&format!("- Layers: `{}`\n", j.layers_path));
    }
    md
}

fn truncate_id(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    format!("{}…", &s[..max.saturating_sub(1)])
}
