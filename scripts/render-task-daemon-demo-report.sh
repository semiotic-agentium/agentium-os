#!/usr/bin/env bash
set -euo pipefail

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required but not found in PATH" >&2
  exit 1
fi

if [ "$#" -lt 8 ]; then
  cat >&2 <<'USAGE'
Usage:
  ./scripts/render-task-daemon-demo-report.sh \
    <context_id> <channel> <extractor> <coordinator_url> \
    <jsonl_out> <metrics_out> <mermaid_out> <report_out>
USAGE
  exit 1
fi

CONTEXT_ID="$1"
CHANNEL="$2"
EXTRACTOR="$3"
COORDINATOR_URL="$4"
JSONL_OUT="$5"
METRICS_OUT="$6"
MERMAID_OUT="$7"
REPORT_OUT="$8"

TURNS="0"
USER_PROMPTS="0"
LLM_CALLS="0"
LLM_DURATION_MS="0"
TOKENS_IN="0"
TOKENS_OUT="0"
TOKENS_TOTAL="0"

if [ -s "$METRICS_OUT" ]; then
  read -r TURNS USER_PROMPTS LLM_CALLS LLM_DURATION_MS TOKENS_IN TOKENS_OUT TOKENS_TOTAL < <(
    jq -r '[
      .session.turns_total // 0,
      .session.user_prompts_total // 0,
      .session.llm_calls_total // 0,
      .session.llm_duration_ms_total // 0,
      .session.tokens_total.in // 0,
      .session.tokens_total.out // 0,
      .session.tokens_total.total // 0
    ] | @tsv' "$METRICS_OUT"
  )
fi

MESSAGES_SCANNED="0"
DERIVED_TASKS="0"
EXEC_SUMMARY_HTML="No interpretation batch was captured."
TASK_ITEMS_HTML="<li>No derived tasks captured.</li>"

if [ -s "$JSONL_OUT" ]; then
  MESSAGES_SCANNED="$(jq -s 'map(.messages_scanned // 0) | add // 0' "$JSONL_OUT")"
  DERIVED_TASKS="$(jq -s 'map((.derived_tasks // []) | length) | add // 0' "$JSONL_OUT")"
  EXEC_SUMMARY_HTML="$(
    jq -s -r 'last.interpretation.executive_summary // "No executive summary available."' "$JSONL_OUT" \
      | jq -Rs -r '@html'
  )"
  TASK_ITEMS_HTML="$(
    jq -s -r '
      def display_title($task):
        if (($task.title // "") == "Blocking clarification") and (($task.description // "") | length > 0) then
          "Clarification needed: " + ($task.description // "")
        else
          ($task.title // "(untitled)")
        end;

      def shorten($text):
        if ($text | length) > 140 then
          ($text[0:137] + "...")
        else
          $text
        end;

      def dedupe_by_display:
        reduce .[] as $task (
          [];
          if any(.[]; display_title(.) == display_title($task)) then . else . + [$task] end
        );

      def normalized_priority($task):
        (($task.priority // "unknown") | ascii_downcase) as $priority
        | if ($priority == "high" or $priority == "medium" or $priority == "low") then
            $priority
          else
            "unknown"
          end;

      if (last.derived_tasks // [] | length) == 0 then
        "<li>No derived tasks captured.</li>"
      else
        (last.derived_tasks // [] | dedupe_by_display | .[0:6][])
        | "<li><span class=\"pill " + normalized_priority(.) + "\">"
          + (normalized_priority(.) | ascii_upcase)
          + "</span><span class=\"task-title\">"
          + (shorten(display_title(.)) | @html)
          + "</span></li>"
      end
    ' "$JSONL_OUT"
  )"
fi

TURN_ROWS_HTML="<tr><td colspan=\"4\">No turn-level metrics captured.</td></tr>"
if [ -s "$METRICS_OUT" ]; then
  TURN_ROWS_HTML="$(
    jq -r '
      if (.turns // [] | length) == 0 then
        "<tr><td colspan=\"4\">No turn-level metrics captured.</td></tr>"
      else
        .turns[]
        | "<tr><td><code>"
          + ((.message_id // "n/a") | @html)
          + "</code></td><td>"
          + ((.llm_call_count // 0) | tostring)
          + "</td><td>"
          + ((.llm_duration_ms_total // 0) | tostring)
          + "</td><td>"
          + ((.tokens.total // 0) | tostring)
          + "</td></tr>"
      end
    ' "$METRICS_OUT"
  )"
fi

MERMAID_SOURCE=$'sequenceDiagram\n  participant Demo\n  Demo->>Demo: Mermaid source unavailable'
if [ -s "$MERMAID_OUT" ]; then
  MERMAID_SOURCE="$(cat "$MERMAID_OUT")"
fi
MERMAID_B64="$(printf '%s' "$MERMAID_SOURCE" | base64 | tr -d '\n')"
MERMAID_SOURCE_HTML="$(printf '%s' "$MERMAID_SOURCE" | jq -Rs -r '@html')"

METRICS_ENDPOINT="${COORDINATOR_URL}/contexts/${CONTEXT_ID}/metrics"
MERMAID_ENDPOINT="${COORDINATOR_URL}/contexts/${CONTEXT_ID}/mermaid"
CONTEXT_ID_HTML="$(printf '%s' "$CONTEXT_ID" | jq -Rs -r '@html')"
CHANNEL_HTML="$(printf '%s' "$CHANNEL" | jq -Rs -r '@html')"
EXTRACTOR_HTML="$(printf '%s' "$EXTRACTOR" | jq -Rs -r '@html')"
COORDINATOR_URL_HTML="$(printf '%s' "$COORDINATOR_URL" | jq -Rs -r '@html')"
METRICS_ENDPOINT_HTML="$(printf '%s' "$METRICS_ENDPOINT" | jq -Rs -r '@html')"
MERMAID_ENDPOINT_HTML="$(printf '%s' "$MERMAID_ENDPOINT" | jq -Rs -r '@html')"

mkdir -p "$(dirname "$REPORT_OUT")"
cat >"$REPORT_OUT" <<EOF
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Task Daemon Demo Report</title>
  <style>
    :root {
      --bg: #f6f7fb;
      --card: #ffffff;
      --ink: #0f172a;
      --muted: #475569;
      --line: #dbe2ee;
      --brand: #1e3a8a;
      --accent: #0f766e;
      --high: #b91c1c;
      --medium: #b45309;
      --low: #1d4ed8;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      background: linear-gradient(180deg, #eef2ff 0%, var(--bg) 28%, var(--bg) 100%);
      color: var(--ink);
      font-family: ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      line-height: 1.45;
    }
    main { max-width: 1200px; margin: 0 auto; padding: 24px 24px 64px; }
    h1, h2, h3 { margin: 0 0 10px; letter-spacing: -0.01em; }
    h1 { font-size: 30px; }
    h2 { font-size: 20px; margin-top: 6px; }
    h3 { font-size: 16px; margin-top: 4px; }
    p { margin: 0 0 12px; color: var(--muted); }
    .card {
      background: var(--card);
      border: 1px solid var(--line);
      border-radius: 14px;
      box-shadow: 0 10px 24px rgba(15, 23, 42, 0.06);
      padding: 18px 20px;
    }
    .hero {
      display: grid;
      grid-template-columns: 1.2fr 1fr;
      gap: 16px;
      margin-bottom: 16px;
    }
    .meta { display: grid; gap: 8px; font-size: 14px; color: var(--muted); }
    .meta code { color: var(--ink); font-size: 13px; }
    .kpi-grid {
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 10px;
      margin-top: 8px;
    }
    .kpi {
      border: 1px solid var(--line);
      border-radius: 10px;
      padding: 10px 12px;
      background: #fafbff;
    }
    .kpi .label { font-size: 12px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.06em; }
    .kpi .value { font-size: 26px; font-weight: 700; color: var(--brand); }
    .grid-2 {
      display: grid;
      grid-template-columns: 1fr 1fr;
      gap: 16px;
      margin-bottom: 16px;
    }
    ul { margin: 0; padding-left: 0; list-style: none; display: grid; gap: 8px; }
    li {
      display: flex;
      align-items: center;
      gap: 10px;
      padding: 8px 10px;
      border: 1px solid var(--line);
      border-radius: 10px;
      background: #fcfdff;
    }
    .pill {
      font-size: 11px;
      font-weight: 700;
      letter-spacing: 0.05em;
      padding: 3px 7px;
      border-radius: 999px;
      color: #fff;
      min-width: 62px;
      text-align: center;
    }
    .pill.high { background: var(--high); }
    .pill.medium { background: var(--medium); }
    .pill.low { background: var(--low); }
    .pill.unknown { background: #64748b; }
    .task-title { color: var(--ink); }
    table {
      width: 100%;
      border-collapse: collapse;
      font-size: 14px;
      margin-top: 6px;
    }
    th, td {
      border-bottom: 1px solid var(--line);
      text-align: left;
      padding: 8px 6px;
      vertical-align: top;
    }
    th { color: var(--muted); font-weight: 600; }
    .diagram-wrap { border: 1px solid var(--line); border-radius: 10px; padding: 8px; background: #fff; overflow-x: auto; }
    .small { font-size: 12px; color: var(--muted); }
    .links a { color: var(--accent); text-decoration: none; }
    details { margin-top: 12px; }
    summary { cursor: pointer; color: var(--brand); font-weight: 600; }
    pre {
      white-space: pre-wrap;
      background: #0b1220;
      color: #dbeafe;
      padding: 12px;
      border-radius: 10px;
      overflow-x: auto;
      font-size: 12px;
    }
    pre.mermaid {
      background: #ffffff;
      color: var(--ink);
      padding: 0;
      border-radius: 0;
      overflow: visible;
    }
    .pipeline-diagram {
      display: flex;
      justify-content: center;
      padding: 8px 0;
    }
    .pipeline-steps {
      display: flex;
      flex-direction: column;
      align-items: center;
      gap: 10px;
    }
    .pipeline-step {
      width: min(540px, 92%);
      text-align: center;
      padding: 12px 14px;
      border: 2px solid #1e3a8a;
      border-radius: 12px;
      background: #ffffff;
      color: #0f172a;
      font-weight: 700;
      letter-spacing: 0.01em;
      font-size: 18px;
      box-shadow: 0 3px 0 rgba(30, 58, 138, 0.08);
    }
    .pipeline-arrow {
      color: #1e3a8a;
      font-size: 24px;
      line-height: 1;
      font-weight: 900;
    }
    #live-sequence svg {
      width: 100% !important;
      height: auto !important;
    }
    @media (max-width: 980px) {
      .hero, .grid-2 { grid-template-columns: 1fr; }
      .kpi-grid { grid-template-columns: 1fr 1fr; }
    }
  </style>
</head>
<body>
  <main>
    <section class="hero">
      <article class="card">
        <h1>Slack to Action Demo</h1>
        <p>From project conversation to clear, prioritized actions with full traceability.</p>
        <div class="meta">
          <div>Context: <code>${CONTEXT_ID_HTML}</code></div>
          <div>Channel: <code>${CHANNEL_HTML}</code></div>
          <div>Interpretation mode: <code>${EXTRACTOR_HTML}</code></div>
          <div>Coordinator URL: <code>${COORDINATOR_URL_HTML}</code></div>
          <div class="links">Metrics: <a href="${METRICS_ENDPOINT_HTML}">${METRICS_ENDPOINT_HTML}</a></div>
          <div class="links">Mermaid: <a href="${MERMAID_ENDPOINT_HTML}">${MERMAID_ENDPOINT_HTML}</a></div>
        </div>
        <h3 style="margin-top:14px;">Executive Summary</h3>
        <p>${EXEC_SUMMARY_HTML}</p>
      </article>
      <article class="card">
        <h2>Scoreboard</h2>
        <div class="kpi-grid">
          <div class="kpi"><div class="label">Messages Scanned</div><div class="value">${MESSAGES_SCANNED}</div></div>
          <div class="kpi"><div class="label">Derived Tasks</div><div class="value">${DERIVED_TASKS}</div></div>
          <div class="kpi"><div class="label">LLM Calls</div><div class="value">${LLM_CALLS}</div></div>
          <div class="kpi"><div class="label">Turns</div><div class="value">${TURNS}</div></div>
          <div class="kpi"><div class="label">Tokens Total</div><div class="value">${TOKENS_TOTAL}</div></div>
          <div class="kpi"><div class="label">LLM Duration (ms)</div><div class="value">${LLM_DURATION_MS}</div></div>
        </div>
        <p class="small" style="margin-top:10px;">Tokens in/out: ${TOKENS_IN}/${TOKENS_OUT} | User prompts: ${USER_PROMPTS}</p>
      </article>
    </section>

    <section class="grid-2">
      <article class="card">
        <h2>How This Works</h2>
        <p>Discussion is interpreted, shaped into tasks, and handed off for coordinated execution.</p>
        <div class="diagram-wrap">
          <div class="pipeline-diagram">
            <div class="pipeline-steps">
              <div class="pipeline-step">Slack discussion</div>
              <div class="pipeline-arrow">↓</div>
              <div class="pipeline-step">Message intake</div>
              <div class="pipeline-arrow">↓</div>
              <div class="pipeline-step">LLM interpretation</div>
              <div class="pipeline-arrow">↓</div>
              <div class="pipeline-step">Action candidates</div>
              <div class="pipeline-arrow">↓</div>
              <div class="pipeline-step">Coordinator handoff</div>
              <div class="pipeline-arrow">↓</div>
              <div class="pipeline-step">Execution plan</div>
              <div class="pipeline-arrow">↓</div>
              <div class="pipeline-step">Trace and metrics</div>
            </div>
          </div>
        </div>
      </article>
      <article class="card">
        <h2>Top Actions Identified</h2>
        <p>Most important next steps extracted from the conversation.</p>
        <ul>
${TASK_ITEMS_HTML}
        </ul>
      </article>
    </section>

    <section class="card" style="margin-bottom:16px;">
      <h2>Turn Metrics</h2>
      <table>
        <thead>
          <tr>
            <th>Message ID</th>
            <th>LLM Calls</th>
            <th>LLM Duration (ms)</th>
            <th>Tokens Total</th>
          </tr>
        </thead>
        <tbody>
${TURN_ROWS_HTML}
        </tbody>
      </table>
    </section>

    <section class="card">
      <h2>Execution Trace</h2>
      <p>End-to-end timeline showing how the system interpreted and routed this conversation.</p>
      <div class="diagram-wrap">
        <div class="mermaid" id="live-sequence"></div>
      </div>
      <details>
        <summary>View raw sequence source</summary>
        <pre>${MERMAID_SOURCE_HTML}</pre>
      </details>
    </section>
  </main>
  <script type="module">
    const mermaidText = atob("${MERMAID_B64}");
    document.getElementById("live-sequence").textContent = mermaidText;
    try {
      const mermaid = await import("https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs");
      mermaid.default.initialize({
        startOnLoad: true,
        securityLevel: "strict",
        theme: "neutral",
        flowchart: { curve: "basis", htmlLabels: false }
      });
      await mermaid.default.run();
    } catch (error) {
      console.warn("Mermaid CDN load failed; showing raw source only.", error);
    }
  </script>
</body>
</html>
EOF

echo "Wrote demo report: $REPORT_OUT" >&2
