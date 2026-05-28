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
    gathered metric and log evidence. Your job: write one concise
    operator-facing incident report grounded ONLY in the inputs below.

    Inputs:
    - incident_json: alert envelope from Grafana (alertName, service, status,
      severity, startsAt, endsAt, fingerprint, dashboardURL, panelURL).
    - investigator_pack_json: an InvestigatorReport object with metrics,
      log_samples, likely_cause, confidence, open_questions, caveats. The
      `traces` field is deprecated for this demo and should be ignored.
    - links_json: precomputed references — `grafana_dashboard`, `grafana_panel`
      (URLs when Grafana provided them, otherwise "not provided"), and
      `context_id` (the Agentium provenance context id; not a URL).

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
       two lines). Do NOT include a trace findings section. Include caveats
       from investigator_pack_json verbatim as separate bullets.
    5. "References" — render `grafana_dashboard` and `grafana_panel` as
       markdown links ONLY when they look like URLs (start with `http`);
       otherwise show the literal placeholder string ("not provided").
       Show `context_id` as a plain code-formatted value (e.g.
       `context_id: ` `ctx-...`). Do NOT invent URLs for it; operators reach
       the Agentium dashboard via `kubectl port-forward svc/agentium-runner
       18080:18080` and that runbook step belongs in docs, not this report.
    6. "Suggested next actions" — numbered list from the investigator's
       open_questions. If one already asks whether this matches expected demo
       injection, do not add another reminder.

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
