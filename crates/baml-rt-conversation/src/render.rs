//! Render [`crate::episode::Episode`] as grep-friendly plain text with episode-local refs.

use std::fmt::Write as _;

use baml_rt_tools::{archive_read::DEFAULT_TOOL_RESULT_INLINE_LINES, citations::ParsedCitation};

use crate::episode::{
    ArtifactSummary, Episode, EpisodeContent, EpisodeEntry, EpisodeRefPrefix, StepType,
    TerminalStatus,
};

/// Render a full episode document (seven headed sections, namespaced refs).
#[must_use]
pub fn render_episode(ep: &Episode) -> String {
    let mut s = String::new();
    render_meta(ep, &mut s);
    render_prior(ep, &mut s);
    render_goal(ep, &mut s);
    render_transcript(ep, &mut s);
    render_intents(ep, &mut s);
    render_plans(ep, &mut s);
    render_drift(ep, &mut s);
    render_outcome(ep, &mut s);
    s
}

fn render_meta(ep: &Episode, out: &mut String) {
    out.push_str("## episode\n");
    let _ = writeln!(out, "task_id: {}", ep.task_id);
    let _ = writeln!(out, "context_id: {}", ep.context_id);
    let _ = writeln!(out, "agent_id: {}", ep.agent_id);
    let _ = writeln!(out, "ref_prefix: {}", ep.ref_prefix.as_str());
    let _ = writeln!(out, "status: {}", terminal_label(&ep.status));
    let _ = writeln!(out, "started_timestamp_ms: {}", ep.started_timestamp_ms);
    let _ = writeln!(out, "duration_active_ms: {}", ep.duration.active_ms);
    let _ = writeln!(out, "duration_wait_ms: {}", ep.duration.wait_ms);
    let _ = writeln!(out, "duration_wall_clock_ms: {}", ep.duration.wall_clock_ms);
    let _ = writeln!(out, "tokens_prompt: {}", ep.token_summary.prompt_tokens);
    let _ = writeln!(
        out,
        "tokens_completion: {}",
        ep.token_summary.completion_tokens
    );
    let _ = writeln!(out, "tokens_total: {}", ep.token_summary.total_tokens);
    let _ = writeln!(out, "llm_call_count: {}", ep.token_summary.llm_call_count);
    let _ = writeln!(
        out,
        "llm_duration_ms_total: {}",
        ep.token_summary.llm_duration_ms
    );
    out.push('\n');
}

fn terminal_label(t: &TerminalStatus) -> &'static str {
    match t {
        TerminalStatus::Completed => "completed",
        TerminalStatus::Failed => "failed",
        TerminalStatus::Canceled => "canceled",
        TerminalStatus::Rejected => "rejected",
        TerminalStatus::Other(_) => "other",
    }
}

fn render_prior(ep: &Episode, out: &mut String) {
    out.push_str("## prior_context\n");
    if ep.prior_context.is_empty() {
        out.push_str("(empty)\n\n");
        return;
    }
    for e in &ep.prior_context {
        render_entry_line(ep, e, out);
    }
    out.push('\n');
}

fn render_goal(ep: &Episode, out: &mut String) {
    out.push_str("## goal\n");
    render_entry_line(ep, &ep.goal, out);
    out.push('\n');
}

fn render_transcript(ep: &Episode, out: &mut String) {
    out.push_str("## transcript\n");
    for e in &ep.transcript {
        render_entry_line(ep, e, out);
    }
    out.push('\n');
}

fn render_intents(ep: &Episode, out: &mut String) {
    out.push_str("## intents\n");
    if ep.intents.is_empty() {
        out.push_str("(empty)\n\n");
        return;
    }
    let p = &ep.ref_prefix;
    for i in &ep.intents {
        let _ = writeln!(
            out,
            "intent_id: {} anchor: {} t={}ms superseded_next={} supersession_prev={:?}",
            i.intent_id,
            i.activity_anchor,
            i.timestamp_ms,
            i.superseded_by_next,
            i.supersession_from_previous
        );
        let _ = writeln!(out, "  description: {}", i.description);
        if !i.derived_citation_strings.is_empty() {
            out.push_str("  citations:");
            for c in &i.derived_citation_strings {
                out.push(' ');
                out.push_str(&prefix_wire_citation(c, p));
            }
            out.push('\n');
        }
    }
    out.push('\n');
}

fn render_plans(ep: &Episode, out: &mut String) {
    out.push_str("## plans\n");
    if ep.plans.is_empty() {
        out.push_str("(empty)\n\n");
        return;
    }
    let p = &ep.ref_prefix;
    for pl in &ep.plans {
        let _ = writeln!(
            out,
            "plan_id: {} intent_id: {} anchor: {} t={}ms superseded_next={}",
            pl.plan_id, pl.intent_id, pl.activity_anchor, pl.timestamp_ms, pl.superseded_by_next
        );
        for st in &pl.steps {
            let _ = writeln!(
                out,
                "  step_id: {} status: {} — {}",
                st.step_id, st.status, st.description
            );
            if !st.citation_strings.is_empty() {
                out.push_str("    citations:");
                for c in &st.citation_strings {
                    out.push(' ');
                    out.push_str(&prefix_wire_citation(c, p));
                }
                out.push('\n');
            }
        }
    }
    out.push('\n');
}

/// Write `key: value\n` for an optional f32 field. No-op when `val` is `None`.
fn write_opt_f32_line(out: &mut String, key: &str, val: Option<f32>) {
    if let Some(v) = val {
        let _ = writeln!(out, "{key}: {v:.2}");
    }
}

/// Append ` key=value` inline (no newline) for an optional f32 field. No-op when `val` is `None`.
fn write_opt_f32_inline(out: &mut String, key: &str, val: Option<f32>) {
    if let Some(v) = val {
        let _ = write!(out, " {key}={v:.2}");
    }
}

fn render_drift(ep: &Episode, out: &mut String) {
    if ep.drift_summary.is_none() && ep.drift_calls.is_empty() {
        return;
    }
    out.push_str("## drift\n");

    if let Some(ds) = &ep.drift_summary {
        let _ = writeln!(out, "composite_severity: {}", ds.composite_severity);
        let _ = writeln!(out, "intent_alignment: {:.2}", ds.intent_alignment);
        write_opt_f32_line(out, "step_alignment", ds.step_alignment);
        write_opt_f32_line(out, "trajectory_drift", ds.trajectory_drift);
        let _ = writeln!(out, "plan_adherence: {:.2}", ds.plan_adherence_score);
        let _ = writeln!(out, "scored_calls: {}", ds.scored_call_count);
        let _ = writeln!(out, "warn_calls: {}", ds.warn_count);
        let _ = writeln!(out, "block_calls: {}", ds.block_count);
        out.push('\n');
    }

    if !ep.drift_calls.is_empty() {
        out.push_str("calls:\n");
        for call in &ep.drift_calls {
            let _ = write!(
                out,
                "  {} function={} severity={}",
                call.activity_anchor, call.function_name, call.severity
            );
            let _ = write!(out, " intent={:.2}", call.intent_alignment);
            write_opt_f32_inline(out, "step", call.step_alignment);
            write_opt_f32_inline(out, "xe", call.cross_encoder_step_score);
            write_opt_f32_inline(out, "traj", call.trajectory_drift);
            let _ = write!(out, " adherence={:.2}", call.plan_adherence_score);
            write_opt_f32_inline(out, "cite_mean", call.citation_mean_similarity);
            write_opt_f32_inline(out, "cite_cov", call.citation_coverage);
            out.push('\n');
        }
        out.push('\n');
    }
}

fn render_outcome(ep: &Episode, out: &mut String) {
    out.push_str("## outcome\n");
    if let Some(m) = &ep.outcome.final_message {
        let _ = writeln!(out, "final_message: {m}");
    } else {
        out.push_str("final_message: (none)\n");
    }
    if ep.outcome.artifacts.is_empty() {
        out.push_str("artifacts: (none)\n");
    } else {
        out.push_str("artifacts:\n");
        for ArtifactSummary { name, media_type } in &ep.outcome.artifacts {
            match media_type {
                Some(mt) => {
                    let _ = writeln!(out, "  - {name} ({mt})");
                }
                None => {
                    let _ = writeln!(out, "  - {name}");
                }
            }
        }
    }
    if ep.outcome.citation_strings.is_empty() {
        out.push_str("citations: (none)\n");
    } else {
        out.push_str("citations:");
        let p = &ep.ref_prefix;
        for c in &ep.outcome.citation_strings {
            out.push(' ');
            out.push_str(&prefix_wire_citation(c, p));
        }
        out.push('\n');
    }
    let _ = writeln!(
        out,
        "summary_tokens_total: {}",
        ep.outcome.token_summary.total_tokens
    );
    let _ = writeln!(
        out,
        "summary_duration_wall_clock_ms: {}",
        ep.outcome.duration.wall_clock_ms
    );
    out.push('\n');
}

fn render_entry_line(ep: &Episode, e: &EpisodeEntry, out: &mut String) {
    let p = ep.ref_prefix.as_str();
    let ts = format_relative_ms(e.elapsed_ms);
    let ref_token = entry_ref_token(e, p);
    let _ = write!(out, "{ref_token} [{ts}] role={} ", e.role);
    match &e.content {
        EpisodeContent::Text(t) => {
            let _ = writeln!(out, "message: {t}");
            if !e.citation_strings.is_empty() {
                out.push_str("  citations:");
                for c in &e.citation_strings {
                    out.push(' ');
                    out.push_str(c);
                }
                out.push('\n');
            }
        }
        EpisodeContent::ToolInvocation {
            tool_name,
            description,
        } => {
            let _ = writeln!(out, "tool_call {tool_name}: {description}");
        }
        EpisodeContent::ToolOutput {
            tool_name,
            summary,
            line_count,
            byte_count,
            lines,
        } => {
            let _ = writeln!(
                out,
                "tool_result {tool_name}: {summary} [{line_count} lines, {byte_count} bytes]"
            );
            let show = lines.len().min(DEFAULT_TOOL_RESULT_INLINE_LINES);
            for line in lines.iter().take(show) {
                let _ = writeln!(out, "  | {line}");
            }
            if lines.len() > show {
                let _ = writeln!(
                    out,
                    "  | … {} more lines (see {}@{})",
                    lines.len() - show,
                    p,
                    e.seq
                );
                let _ = writeln!(
                    out,
                    "  # lines 1-{show} of {} ({} more — offset={show} for next page)",
                    lines.len(),
                    lines.len().saturating_sub(show),
                );
            }
        }
        EpisodeContent::PlanRevisionRef { summary } => {
            let _ = writeln!(out, "plan_revision: {summary}");
        }
        EpisodeContent::StatusChange { old, new, message } => match message {
            Some(m) => {
                let _ = writeln!(out, "status: {old} -> {new} ({m})");
            }
            None => {
                let _ = writeln!(out, "status: {old} -> {new}");
            }
        },
        EpisodeContent::Artifact {
            name,
            media_type,
            size_bytes,
        } => match (media_type, size_bytes) {
            (Some(mt), Some(sz)) => {
                let _ = writeln!(out, "artifact: {name} ({mt}) size={sz}");
            }
            (Some(mt), None) => {
                let _ = writeln!(out, "artifact: {name} ({mt})");
            }
            (None, Some(sz)) => {
                let _ = writeln!(out, "artifact: {name} size={sz}");
            }
            (None, None) => {
                let _ = writeln!(out, "artifact: {name}");
            }
        },
    }
}

fn entry_ref_token(e: &EpisodeEntry, prefix: &str) -> String {
    match e.step_type {
        StepType::ToolResult => format!("{prefix}@{}", e.seq),
        StepType::ToolRead => match &e.content {
            // Expanded archive body: keep `@seq` (tool-output row), same as ToolResult.
            EpisodeContent::ToolOutput { .. } => format!("{prefix}@{}", e.seq),
            // Explicit session Read op: invocation row uses `#seq`.
            _ => format!("{prefix}#{}", e.seq),
        },
        StepType::Message
        | StepType::ToolCall
        | StepType::PlanRevision
        | StepType::StatusTransition
        | StepType::ArtifactEmitted => format!("{prefix}#{}", e.seq),
    }
}

fn format_relative_ms(elapsed_ms: i64) -> String {
    if elapsed_ms < 0 {
        format!("Δ{elapsed_ms}ms")
    } else {
        format!("+{elapsed_ms}ms")
    }
}

/// Rewrite a session citation (`#1`, `@2:3`) into episode-local form (`abcd#1`, `abcd@2:3`).
#[must_use]
pub fn prefix_wire_citation(raw: &str, prefix: &EpisodeRefPrefix) -> String {
    let s = raw.trim();
    match ParsedCitation::parse(s) {
        Ok(parsed) => format_parsed_episode_citation(&parsed, prefix),
        Err(_) => raw.to_string(),
    }
}

fn is_citation_boundary_prev(ch: Option<char>) -> bool {
    !matches!(ch, Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

/// Prefix every wire `#N` / `@N` citation token in free text with `prefix` (episode-local namespace).
#[must_use]
pub fn prefix_wire_citations_in_text(s: &str, prefix: &EpisodeRefPrefix) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(32));
    let mut i = 0usize;
    while i < s.len() {
        let prev = if i == 0 {
            None
        } else {
            s[..i].chars().next_back()
        };
        if is_citation_boundary_prev(prev) {
            let slice = &s[i..];
            if let Some((parsed, consumed)) = try_parse_wire_citation_prefix(slice) {
                out.push_str(&format_parsed_episode_citation(&parsed, prefix));
                i += consumed;
                continue;
            }
        }
        let ch = s[i..].chars().next().expect("i < len");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn try_parse_wire_citation_prefix(s: &str) -> Option<(ParsedCitation, usize)> {
    let mut total = 0usize;
    let (body, neg) = if let Some(r) = s.strip_prefix('!') {
        total += 1;
        (r, true)
    } else {
        (s, false)
    };
    if body.is_empty() {
        return None;
    }
    let b = body.as_bytes();
    let first = *b.first()?;
    if first != b'#' && first != b'@' {
        return None;
    }
    let mut end = 1usize;
    while end < b.len() && b[end].is_ascii_digit() {
        end += 1;
    }
    if end == 1 {
        return None;
    }
    if first == b'@' && end < b.len() && b[end] == b':' {
        end += 1;
        let start_line = end;
        while end < b.len() && b[end].is_ascii_digit() {
            end += 1;
        }
        if end == start_line {
            return None;
        }
        if end < b.len() && b[end] == b'-' {
            end += 1;
            let start2 = end;
            while end < b.len() && b[end].is_ascii_digit() {
                end += 1;
            }
            if end == start2 {
                return None;
            }
        }
    }
    let token = if neg {
        format!("!{}", &body[..end])
    } else {
        body[..end].to_string()
    };
    let parsed = ParsedCitation::parse(&token).ok()?;
    Some((parsed, total + end))
}

fn format_parsed_episode_citation(c: &ParsedCitation, prefix: &EpisodeRefPrefix) -> String {
    let p = prefix.as_str();
    match c {
        ParsedCitation::History { n, negated } => {
            let core = format!("{p}#{n}");
            if *negated { format!("!{core}") } else { core }
        }
        ParsedCitation::Archive { n, lines, negated } => {
            let mut core = format!("{p}@{n}");
            if let Some(r) = lines {
                core.push(':');
                if r.start() == r.end() {
                    core.push_str(&r.start().to_string());
                } else {
                    core.push_str(&r.start().to_string());
                    core.push('-');
                    core.push_str(&r.end().to_string());
                }
            }
            if *negated { format!("!{core}") } else { core }
        }
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::{AgentId, ContextId, ExternalId, TaskId, UuidId};
    use uuid::Uuid;

    use super::{prefix_wire_citation, prefix_wire_citations_in_text, render_episode};
    use crate::episode::{
        Episode, EpisodeContent, EpisodeDuration, EpisodeEntry, EpisodeOutcome, EpisodeRefPrefix,
        StepType, TerminalStatus, TokenSummary,
    };

    #[test]
    fn prefix_citation_history_and_archive() {
        let p = EpisodeRefPrefix::from_task_id(&TaskId::from_external(ExternalId::new("t1")));
        assert_eq!(prefix_wire_citation("#3", &p), format!("{}#3", p.as_str()));
        assert_eq!(
            prefix_wire_citation("!@4:1-3", &p),
            format!("!{}@4:1-3", p.as_str())
        );
    }

    #[test]
    fn prefix_citations_embedded_in_session_line() {
        let p = EpisodeRefPrefix::from_task_id(&TaskId::from_external(ExternalId::new("t1")));
        let s = prefix_wire_citations_in_text("#1 hello grep -n 'x' @2", &p);
        assert!(s.contains(&format!("{}#1", p.as_str())));
        assert!(s.contains(&format!("{}@2", p.as_str())));
    }

    #[test]
    fn render_episode_smoke() {
        let task_id = TaskId::from_external(ExternalId::new("task-a"));
        let ep = Episode {
            task_id: task_id.clone(),
            context_id: ContextId::new(1, 1),
            agent_id: AgentId::from_uuid(UuidId::new(Uuid::nil())),
            ref_prefix: EpisodeRefPrefix::from_task_id(&task_id),
            status: TerminalStatus::Completed,
            started_timestamp_ms: 100,
            duration: EpisodeDuration {
                active_ms: 10,
                wait_ms: 2,
                wall_clock_ms: 12,
            },
            token_summary: TokenSummary::default(),
            prior_context: vec![],
            goal: EpisodeEntry {
                seq: 1,
                step_type: StepType::Message,
                role: "user".into(),
                elapsed_ms: 0,
                content: EpisodeContent::Text("do the thing".into()),
                activity_anchor: "a1".into(),
                citation_strings: vec![],
            },
            transcript: vec![EpisodeEntry {
                seq: 1,
                step_type: StepType::Message,
                role: "user".into(),
                elapsed_ms: 0,
                content: EpisodeContent::Text("do the thing".into()),
                activity_anchor: "a1".into(),
                citation_strings: vec![],
            }],
            session_history: vec![],
            drift_summary: None,
            drift_calls: vec![],
            intents: vec![],
            plans: vec![],
            outcome: EpisodeOutcome {
                final_message: Some("done".into()),
                artifacts: vec![],
                citation_strings: vec![],
                token_summary: TokenSummary::default(),
                duration: EpisodeDuration::default(),
            },
        };
        let txt = render_episode(&ep);
        assert!(txt.contains("## episode"));
        assert!(txt.contains("## transcript"));
        assert!(txt.contains("## outcome"));
        assert!(txt.contains("do the thing"));
    }

    #[test]
    fn tool_output_overflow_adds_offset_hint() {
        use baml_rt_tools::archive_read::DEFAULT_TOOL_RESULT_INLINE_LINES;
        let task_id = TaskId::from_external(ExternalId::new("task-c"));
        let line_strings: Vec<String> = (0..DEFAULT_TOOL_RESULT_INLINE_LINES + 12)
            .map(|i| format!("content line {i}"))
            .collect();
        let n_lines = line_strings.len();
        let ep = Episode {
            task_id: task_id.clone(),
            context_id: ContextId::new(1, 1),
            agent_id: AgentId::from_uuid(UuidId::new(Uuid::nil())),
            ref_prefix: EpisodeRefPrefix::from_task_id(&task_id),
            status: TerminalStatus::Completed,
            started_timestamp_ms: 100,
            duration: EpisodeDuration {
                active_ms: 10,
                wait_ms: 2,
                wall_clock_ms: 12,
            },
            token_summary: TokenSummary::default(),
            prior_context: vec![],
            goal: EpisodeEntry {
                seq: 1,
                step_type: StepType::Message,
                role: "user".into(),
                elapsed_ms: 0,
                content: EpisodeContent::Text("go".into()),
                activity_anchor: "a0".into(),
                citation_strings: vec![],
            },
            transcript: vec![EpisodeEntry {
                seq: 2,
                step_type: StepType::ToolResult,
                role: "tool".into(),
                elapsed_ms: 1,
                content: EpisodeContent::ToolOutput {
                    tool_name: "big/tool".into(),
                    summary: "s".into(),
                    line_count: n_lines,
                    byte_count: 999,
                    lines: line_strings,
                },
                activity_anchor: "a2".into(),
                citation_strings: vec![],
            }],
            session_history: vec![],
            drift_summary: None,
            drift_calls: vec![],
            intents: vec![],
            plans: vec![],
            outcome: EpisodeOutcome {
                final_message: Some("done".into()),
                artifacts: vec![],
                citation_strings: vec![],
                token_summary: TokenSummary::default(),
                duration: EpisodeDuration::default(),
            },
        };
        let txt = render_episode(&ep);
        assert!(
            txt.contains("more — offset="),
            "expected numeric offset footer in:\n{txt}"
        );
        assert!(
            txt.contains("for next page"),
            "expected paging hint in:\n{txt}"
        );
    }
}
