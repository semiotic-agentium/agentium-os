/// <reference path="./baml-runtime.d.ts" />
/**
 * Frontend Expert Agent
 * --------------------
 * Specialist agent for frontend development: proposes UI features, reviews
 * Vue/CSS code, and provides structured code proposals for the Agentium dashboard.
 *
 * Flow: AnalyzeFrontend (structured JSON) → emit artifact → ChooseClaudeDevAction
 *       loop (applies changes via claude/dev) → FormatProposal (readable summary).
 */
import type { FrontendProposal, SessionResult } from "./baml-runtime";

// --- ChooseClaudeDevAction return type guards ---
type ClaudeDevAskUser = { action: string; prompt: string };
type ClaudeDevReport = { action: string; message: string };
type ClaudeCompletion = "DONE" | "INPUT_REQUIRED" | "INTERRUPTED";
type ClaudeEvent = {
  kind?: string;
  text?: string;
  thinking?: string;
  subtype?: string;
  result?: string;
  is_error?: boolean;
  [key: string]: unknown;
};
type ClaudeNextOutput = { events?: ClaudeEvent[]; completion?: ClaudeCompletion };

function isObject(v: unknown): v is Record<string, unknown> {
  return v != null && typeof v === "object";
}

function isClaudeDevAskUser(v: unknown): v is ClaudeDevAskUser {
  return (
    isObject(v) &&
    (v as ClaudeDevAskUser).action === "AskUser" &&
    typeof (v as ClaudeDevAskUser).prompt === "string"
  );
}

function isClaudeDevReport(v: unknown): v is ClaudeDevReport {
  return (
    isObject(v) &&
    (v as ClaudeDevReport).action === "Report" &&
    typeof (v as ClaudeDevReport).message === "string"
  );
}

function asNextOutput(value: unknown): ClaudeNextOutput {
  if (isObject(value) && Array.isArray((value as ClaudeNextOutput).events)) {
    return value as ClaudeNextOutput;
  }
  if (isObject(value) && isObject((value as { output?: unknown }).output)) {
    const out = (value as { output: ClaudeNextOutput }).output;
    if (Array.isArray(out.events)) return out;
  }
  return {};
}

function asChunkArray(raw: unknown): ClaudeNextOutput[] {
  if (Array.isArray(raw) && raw.length > 0) {
    return raw.map((v) => asNextOutput(v));
  }
  if (raw != null) {
    const one = asNextOutput(raw);
    if (one.events != null || one.completion != null) return [one];
  }
  return [];
}

function renderEvents(events: ClaudeEvent[]): string {
  const lines: string[] = [];
  for (const event of events) {
    const kind = event.kind || "unknown";
    if (kind === "assistant_text" && typeof event.text === "string") {
      lines.push(event.text);
    } else if (kind === "terminal_result" && typeof event.result === "string" && event.result.length > 0) {
      lines.push(event.result);
    }
  }
  return lines.join("\n").trim();
}

function formatLastToolOutput(chunks: ClaudeNextOutput[]): string {
  const parts = chunks.map((next) => {
    const rendered = renderEvents(next.events || []);
    const completion = next.completion ?? "";
    return `[output]\n${rendered}\n[completion] ${completion}`.trim();
  });
  return parts.join("\n\n").trim() || "";
}

const MAX_DEV_ACTIONS = 40;

declare function ChooseClaudeDevAction(args: {
  spec_text: string;
  validation_criteria_json: string;
  last_tool_output: string;
  user_approval_intent: string;
}): Promise<unknown>;

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    const text = (ctx.text ?? "").trim();
    if (!text) {
      return {
        message:
          "I'm the frontend expert for the Agentium dashboard. Describe a UI feature, design improvement, or code change you'd like me to analyze and I'll provide a structured proposal with implementable code.",
      };
    }

    try {
      ctx.emit.message("Analyzing your frontend request...");

      // Step 1: Get structured proposal from LLM
      const proposal: FrontendProposal = await AnalyzeFrontend({ user_message: text });

      // Step 2: Emit structured artifact (JSON with file paths + code)
      ctx.emit.artifact(
        {
          name: "FrontendProposal",
          description: `Structured code proposal: ${proposal.summary.slice(0, 80)}`,
          parts: [
            {
              mediaType: "application/json",
              data: JSON.stringify(proposal, null, 2),
            },
          ],
        },
        false,
        true,
      );

      // Step 3: Build spec from proposal for claude/dev
      const specText = [
        "Apply the following frontend code changes to the Agentium dashboard (web/src/).",
        "",
        `Summary: ${proposal.summary}`,
        "",
        ...proposal.proposals.map(
          (p, i) =>
            `Change ${i + 1}: [${p.change_type}] ${p.file_path}\n${p.description}\n\`\`\`\n${p.code_snippet}\n\`\`\``
        ),
        "",
        `Impact: ${proposal.impact_assessment}`,
        proposal.accessibility_notes ? `Accessibility: ${proposal.accessibility_notes}` : "",
      ].join("\n");

      const validationCriteria = [
        "All modified files pass vue-tsc type-checking",
        "Vite build completes without errors",
        "Both light and dark themes render correctly",
        "Mobile responsive at 640px breakpoint",
      ];

      ctx.emit.message("Applying code changes via claude/dev...");
      ctx.emit.statusChanged("TASK_STATE_WORKING");
      await Promise.resolve();

      // Step 4: Drive claude/dev session to apply the changes
      let lastToolOutput = "";
      let userApprovalIntent = "";

      for (let step = 0; step < MAX_DEV_ACTIONS; step++) {
        const result = await ChooseClaudeDevAction({
          spec_text: specText,
          validation_criteria_json: JSON.stringify(validationCriteria),
          last_tool_output: lastToolOutput,
          user_approval_intent: userApprovalIntent,
        });

        if (isClaudeDevReport(result)) {
          // Dev session complete — format and return
          const summary = await FormatProposal({ user_message: text, proposal });
          return { message: `${String(summary)}\n\n---\n${result.message}` };
        }

        if (isClaudeDevAskUser(result)) {
          if (lastToolOutput === "") {
            // Retry — shouldn't ask user on first call
            const retryResult = await ChooseClaudeDevAction({
              spec_text: specText,
              validation_criteria_json: JSON.stringify(validationCriteria),
              last_tool_output: lastToolOutput,
              user_approval_intent: userApprovalIntent,
            });
            if (isClaudeDevReport(retryResult)) {
              const summary = await FormatProposal({ user_message: text, proposal });
              return { message: `${String(summary)}\n\n---\n${retryResult.message}` };
            }
            if (!isClaudeDevAskUser(retryResult)) {
              lastToolOutput = formatLastToolOutput(asChunkArray(retryResult));
              continue;
            }
          }
          ctx.emit.message(result.prompt);
          const reply = await ctx.emit.awaitInput("");
          const parts = (reply as { parts?: unknown[] })?.parts;
          if (Array.isArray(parts)) {
            for (const part of parts) {
              if (isObject(part)) {
                const approval = (part as { toolApproval?: { approved?: boolean } }).toolApproval;
                if (approval && typeof approval.approved === "boolean") {
                  userApprovalIntent = approval.approved ? "approved" : "rejected";
                }
              }
            }
          }
          continue;
        }

        // Session plan was executed by runtime — accumulate output
        lastToolOutput = formatLastToolOutput(asChunkArray(result));
      }

      return { error: "Development session exceeded maximum actions." };
    } catch (e) {
      const errMsg = e instanceof Error ? e.message : String(e);
      return { error: errMsg };
    }
  },
});
