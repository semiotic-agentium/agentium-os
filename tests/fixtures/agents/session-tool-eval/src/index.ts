/// <reference path="./baml-runtime.d.ts" />

type SessionContext = {
  contract_version: "session_context";
  session_open: boolean;
  status_token: "X" | "O" | "S" | "D" | "F";
  allowed_ops: string[];
  last_status: string | null;
};

type ToolSessionHandle = {
  send(args: Record<string, unknown>): Promise<unknown>;
  continue(readInput?: Record<string, unknown>): Promise<unknown>;
  finish(): Promise<unknown>;
  abort(reason?: string): Promise<unknown>;
};

type ParsedStep = {
  status: string;
  output: unknown;
};

function parseStep(raw: unknown): ParsedStep {
  if (raw && typeof raw === "object") {
    const obj = raw as Record<string, unknown>;
    const status = typeof obj.status === "string" ? obj.status.toLowerCase() : "done";
    const output = "output" in obj ? obj.output : raw;
    return { status, output };
  }
  return { status: "done", output: raw };
}

function summarizeOutput(value: unknown): string {
  if (!value || typeof value !== "object") return "no output";
  const obj = value as Record<string, unknown>;
  const level = typeof obj.level === "string" ? obj.level : "unknown";
  const goalId = typeof obj.goal_id === "string" ? obj.goal_id : "none";
  const refs = Array.isArray(obj.refs)
    ? obj.refs.filter((v): v is string => typeof v === "string").slice(0, 6)
    : [];
  return `level=${level}; goal_id=${goalId}; refs=${refs.join(",")}`;
}

function initialContext(): SessionContext {
  return {
    contract_version: "session_context",
    session_open: false,
    status_token: "X",
    allowed_ops: ["Open"],
    last_status: null,
  };
}

declare function openToolSession(
  toolName: string,
  openInput?: Record<string, unknown>,
): Promise<ToolSessionHandle>;

__chat_register({
  run: async (ctx) => {
    const objective =
      (ctx.text || "").trim() ||
      "Find non_trivial_scope_cache_goal using efficient session reads.";

    let sessionContext = initialContext();
    let summary: string | null = null;
    let handle: ToolSessionHandle | null = null;
    let lastOutput: unknown = null;

    try {
      for (let hop = 0; hop < 24; hop++) {
        const plan = await PlanSyntheticSessionStep({
          objective,
          session_context: sessionContext,
          session_state: null,
          output_summary: summary,
        });
        const decision = (plan as { decision?: { op?: string; initial_input?: unknown; input?: unknown } })
          .decision;
        const op = decision?.op;
        if (!op || !sessionContext.allowed_ops.includes(op)) {
          return { error: `planner emitted invalid op '${String(op)}' for allowed_ops=${JSON.stringify(sessionContext.allowed_ops)}` };
        }

        if (op === "Open") {
          const openInput =
            (decision.initial_input && typeof decision.initial_input === "object"
              ? (decision.initial_input as Record<string, unknown>)
              : { reason: "session-tool-eval" });
          handle = await openToolSession("test_eval/synthetic_session_eval", openInput);
          sessionContext = {
            contract_version: "session_context",
            session_open: true,
            status_token: "O",
            allowed_ops: ["Send"],
            last_status: "open",
          };
          continue;
        }

        if (!handle) return { error: "session handle missing after Open" };

        if (op === "Send") {
          const input = (decision.input && typeof decision.input === "object")
            ? (decision.input as Record<string, unknown>)
            : {};
          await handle.send(input);
          sessionContext = {
            contract_version: "session_context",
            session_open: true,
            status_token: "S",
            allowed_ops: ["Read", "Send", "Finish"],
            last_status: "sent",
          };
          continue;
        }

        if (op === "Read") {
          const readInput = (decision.input && typeof decision.input === "object")
            ? (decision.input as Record<string, unknown>)
            : {};
          const parsed = parseStep(await handle.continue(readInput));
          if (parsed.status === "error") {
            return { error: "tool session returned error status on Read" };
          }
          lastOutput = parsed.output;
          summary = summarizeOutput(parsed.output);
          const goalId =
            parsed.output &&
            typeof parsed.output === "object" &&
            typeof (parsed.output as Record<string, unknown>).goal_id === "string"
              ? String((parsed.output as Record<string, unknown>).goal_id)
              : null;
          sessionContext = {
            contract_version: "session_context",
            session_open: true,
            status_token: "D",
            allowed_ops: goalId ? ["Finish"] : ["Read", "Send", "Finish"],
            last_status: "done",
          };
          continue;
        }

        if (op === "Finish") {
          await handle.finish();
          sessionContext = {
            contract_version: "session_context",
            session_open: false,
            status_token: "F",
            allowed_ops: [],
            last_status: "finished",
          };
          const goalId =
            lastOutput &&
            typeof lastOutput === "object" &&
            typeof (lastOutput as Record<string, unknown>).goal_id === "string"
              ? String((lastOutput as Record<string, unknown>).goal_id)
              : "none";
          return {
            message: `session-tool-eval finished; goal_id=${goalId}; ${summary || "no-summary"}`,
          };
        }

        return { error: `unsupported operation '${op}'` };
      }

      return { error: "session-tool-eval exceeded max execution hops" };
    } catch (err) {
      if (handle) {
        try {
          await handle.abort(err instanceof Error ? err.message : String(err));
        } catch {
          // Ignore abort failure on error path.
        }
      }
      return { error: err instanceof Error ? err.message : String(err) };
    }
  },
});
