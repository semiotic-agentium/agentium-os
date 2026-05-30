// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! BAML-style `session_history` lines for an episode, aligned with live `conversation_history` projection.

use baml_rt_tools::{
    archive_read::format_session_read_from_vtable,
    archive_refs::RefTable,
    prompt_projection::{ProjectionRenderOptions, project_projection_item_to_rows},
    tools::ToolRegistry,
};

use crate::{
    episode::{EpisodeRefPrefix, SessionHistoryLine},
    projection::provenance_item_to_projection_item,
    render::prefix_wire_citations_in_text,
    timeline::TimelineKind,
};

/// Assemble session-history lines from a merged task timeline and a pre-built archive [`RefTable`]
/// (use the same construction as transcript assembly, e.g. `episode_ref_table_with_merged` in the
/// provenance layer).
pub fn assemble_session_history(
    merged: &[TimelineKind],
    ref_prefix: &EpisodeRefPrefix,
    tool_registry: &ToolRegistry,
    opts: &ProjectionRenderOptions,
    vtable: &RefTable,
) -> Vec<SessionHistoryLine> {
    let scratch = RefTable::new();

    let archive_reader = |archive_ref_str: &str,
                          grep_str: Option<&str>,
                          offset: usize,
                          limit: usize|
     -> Option<String> {
        format_session_read_from_vtable(vtable, archive_ref_str, grep_str, offset, limit)
    };

    let mut out = Vec::new();
    for m in merged {
        match m {
            TimelineKind::Conv(item, _) => {
                if let Some(proj) = provenance_item_to_projection_item(item.clone()) {
                    for row in project_projection_item_to_rows(
                        &proj,
                        tool_registry,
                        &scratch,
                        Some(&archive_reader),
                        *opts,
                    ) {
                        let cits: Vec<String> = match &row.message_citations {
                            Some(refs) if !refs.is_empty() => refs
                                .iter()
                                .map(|r| prefix_wire_citations_in_text(r, ref_prefix))
                                .collect(),
                            _ => Vec::new(),
                        };
                        out.push(SessionHistoryLine {
                            role: row.role.to_string(),
                            content: prefix_wire_citations_in_text(&row.content, ref_prefix),
                            citations: cits,
                        });
                    }
                }
            }
            TimelineKind::Status(s) => {
                let content = format!("{} → {}", s.old_status, s.new_status);
                out.push(SessionHistoryLine {
                    role: "system".into(),
                    content: prefix_wire_citations_in_text(&content, ref_prefix),
                    citations: vec![],
                });
            }
            TimelineKind::Artifact(a) => {
                let mt = a.media_type.as_deref().unwrap_or("?");
                let content = format!("artifact {} ({})", a.name, mt);
                out.push(SessionHistoryLine {
                    role: "agent".into(),
                    content: prefix_wire_citations_in_text(&content, ref_prefix),
                    citations: vec![],
                });
            }
        }
    }
    out
}
