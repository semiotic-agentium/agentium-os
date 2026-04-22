//! BAML-style `session_history` lines for an episode, aligned with live `conversation_history` projection.

use baml_rt_tools::{
    archive_read::{PageLimit, ShortRef, format_session_read_body_from_rendered},
    archive_refs::RefTable,
    prompt_projection::ProjectionRenderOptions,
    tools::ToolRegistry,
};

use crate::{
    episode::{EpisodeRefPrefix, SessionHistoryLine},
    projection::projection_pairs_for_conv_item,
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

    let archive_reader = move |archive_ref_str: &str,
                               grep_str: Option<&str>,
                               offset: usize,
                               limit: usize|
          -> Option<String> {
        let short_ref = ShortRef::parse_loose(archive_ref_str)?;
        let entry = vtable.get(short_ref)?;
        Some(format_session_read_body_from_rendered(
            &entry.content,
            archive_ref_str,
            grep_str,
            offset,
            PageLimit::new(limit),
        ))
    };

    let mut out = Vec::new();
    for m in merged {
        match m {
            TimelineKind::Conv(item, _) => {
                if let Some(pairs) = projection_pairs_for_conv_item(
                    item,
                    tool_registry,
                    &scratch,
                    Some(&archive_reader),
                    *opts,
                ) {
                    for (role, content) in pairs {
                        out.push(SessionHistoryLine {
                            role,
                            content: prefix_wire_citations_in_text(&content, ref_prefix),
                        });
                    }
                }
            }
            TimelineKind::Status(s) => {
                let content = format!("{} → {}", s.old_status, s.new_status);
                out.push(SessionHistoryLine {
                    role: "system".into(),
                    content: prefix_wire_citations_in_text(&content, ref_prefix),
                });
            }
            TimelineKind::Artifact(a) => {
                let mt = a.media_type.as_deref().unwrap_or("?");
                let content = format!("artifact {} ({})", a.name, mt);
                out.push(SessionHistoryLine {
                    role: "agent".into(),
                    content: prefix_wire_citations_in_text(&content, ref_prefix),
                });
            }
        }
    }
    out
}
