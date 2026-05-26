// Hand-written prompts for the observability coordinator.
// The coordinator delegates evidence gathering to grafana-investigator over A2A and
// then asks an LLM here to write the operator-facing incident report. Tool/A2A
// orchestration stays in TypeScript.

function SynthesizeIncidentReport(
  incident_json: string,
  investigator_pack_json: string,
  links_json: string,
) -> StructuredReply {
  client DefaultClient
  prompt #"
    You are the observability coordinator. The grafana-investigator already
    gathered metric, log, and synthetic trace evidence. Your job: write one
    concise operator-facing incident report grounded ONLY in the inputs below.

    Inputs:
    - incident_json: alert envelope from Grafana (alertName, service, status,
      severity, startsAt, endsAt, fingerprint, dashboardURL, panelURL).
    - investigator_pack_json: an InvestigatorReport object with metrics,
      log_samples, traces, likely_cause, confidence, open_questions, caveats.
    - links_json: precomputed URLs (grafana_dashboard, grafana_panel,
      agentium_dashboard, transcript_api, llm_provenance_api, tool_provenance_api).

    Required report structure (single TextPart, media_type text/markdown):
    1. Header line with alert + service + status emoji (🚨 for firing,
       ✅ for resolved, ⚠️ otherwise).
    2. "Summary" — 2–4 bullets describing impact and what changed, grounded in
       the investigator's metric deltas (e.g. "p95 rose from 120ms baseline to
       1.82s during the window").
    3. "Likely cause" — one sentence verbatim or lightly polished from the
       investigator pack. Include the confidence label in parentheses.
    4. "Evidence" — bullet list. One bullet per metric finding (include the
       PromQL query); one bullet for representative log samples (quote up to
       two lines); one bullet for trace findings (operation, dependency,
       duration_ms). Include the trace-is-synthetic caveat as a separate bullet.
    5. "Links" — markdown links to Grafana dashboard, Grafana panel, Agentium
       dashboard, and the raw provenance APIs from links_json. Use the literal
       string from links_json verbatim; do not invent URLs.
    6. "Suggested next actions" — numbered list from the investigator's
       open_questions; add a closing reminder to confirm whether this matches a
       demo injection if not already covered.

    Hard rules:
    - Do not invent metrics, log lines, span values, or URLs absent from inputs.
    - When investigator_pack_json reports confidence "unknown" or a tool error,
      say so explicitly in the Summary rather than hiding it.
    - For status "resolved", retitle Summary as "Resolution summary" and shorten
      the report — operator only needs confirmation that latency returned to
      baseline and the window length.
    - Citations array: when investigator findings reference archive ids in the
      transcript below, copy those @N refs; otherwise return [].

    incident_json:
    {{ incident_json }}

    investigator_pack_json:
    {{ investigator_pack_json }}

    links_json:
    {{ links_json }}

    {% if ctx.tags['conversation_transcript'] %}
    Session history:
    {{ ctx.tags['conversation_transcript'] }}
    {% endif %}

    {{ ctx.output_format }}

    Return exactly one StructuredReply JSON object.
    parts[] must contain a single TextPart with type "text" and media_type
    "text/markdown". Do not add prose before or after the JSON.
  "#
}

client DefaultClient {
  provider openai-generic
  options {
    model env.BAML_TEST_MODEL
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}
