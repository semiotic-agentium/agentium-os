function SummarizeConversationPrefix(
  transcript_prefix: string,
  active_planning_digest: string?,
  recent_tail_preview: string?,
) -> string {
  client DefaultClient
  prompt #"
    You have been working on a task across a long conversation. The earlier
    transcript below will be removed from your context and replaced by the
    summary you now write. Write the summary for your future self so you can
    continue the task seamlessly without the raw history.

    Preserve precisely:
    - Current state: what has been done and where things stand now
    - Decisions made, and the reasoning behind each
    - Concrete facts established: identifiers, names, values, results
    - Constraints and rules, including any stated only once
    - Open questions and unresolved issues
    - Next steps, and whether the task is near completion

    Be specific over general — keep exact names and values rather than
    paraphrasing them away. Do not invent information not present in the
    transcript. Write dense, plain prose.

    {% if active_planning_digest %}
    Current plan and obligations (must remain actionable in your summary):
    {{ active_planning_digest }}
    {% endif %}

    Transcript to summarize:
    {{ transcript_prefix }}

    {% if recent_tail_preview %}
    The most recent exchanges below are retained in full after your summary —
    do not repeat them, but ensure your summary leads into them coherently:
    {{ recent_tail_preview }}
    {% endif %}
  "#
}
