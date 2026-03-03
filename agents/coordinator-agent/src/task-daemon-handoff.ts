import type { ChatMessage } from "./baml-runtime";

const TASK_DAEMON_COORDINATOR_HANDOFF_SCHEMA_VERSION = "task-daemon.coordinator-handoff.v1";
const MAX_HANDOFF_LIST_ITEMS = 12;
const MAX_HANDOFF_FIELD_CHARS = 700;
const MAX_HANDOFF_PROMPT_CHARS = 12_000;

type TaskDaemonCoordinatorHandoff = {
  schema_version: string;
  batch: Record<string, unknown>;
};

export type PlannerUserTextFromHandoff = {
  userText: string;
  structuredHandoff: boolean;
};

function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

function normalizeText(value: string): string {
  return value.replace(/\s+/g, " ").trim();
}

function normalizeOptionalString(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const normalized = value.trim();
  return normalized.length > 0 ? normalized : null;
}

function parseStringArray(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is string => typeof entry === "string");
}

function parseObjectArray(value: unknown): Record<string, unknown>[] {
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is Record<string, unknown> => isObject(entry));
}

function parseOptionalFiniteNumber(value: unknown): number | null {
  const parsed = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(parsed)) return null;
  return parsed;
}

function parseOptionalBoolean(value: unknown): boolean | null {
  if (typeof value === "boolean") return value;
  return null;
}

function parseObjectField(
  object: Record<string, unknown>,
  key: string,
): Record<string, unknown> | null {
  const value = object[key];
  if (!isObject(value)) return null;
  return value;
}

function truncateForPrompt(text: string, maxChars: number = MAX_HANDOFF_FIELD_CHARS): string {
  if (text.length <= maxChars) return text;
  return `${text.slice(0, Math.max(0, maxChars - 3))}...`;
}

function appendNumberedSection(lines: string[], title: string, entries: string[]): void {
  if (entries.length === 0) return;
  lines.push(`${title}:`);
  const shown = entries.slice(0, MAX_HANDOFF_LIST_ITEMS);
  shown.forEach((entry, index) => {
    lines.push(`${index + 1}. ${truncateForPrompt(entry)}`);
  });
  if (entries.length > shown.length) {
    lines.push(`... ${entries.length - shown.length} more`);
  }
  lines.push("");
}

function formatSourceRef(source: Record<string, unknown>): string | null {
  const permalink = normalizeOptionalString(source.permalink);
  if (permalink) return permalink;
  return normalizeOptionalString(source.reference);
}

function formatTaskSources(task: Record<string, unknown>): string {
  const sourceRefs = parseObjectArray(task.sources)
    .map((source) => formatSourceRef(source))
    .filter((value): value is string => value != null)
    .slice(0, 2);
  if (sourceRefs.length === 0) return "";
  return ` | sources: ${sourceRefs.join(", ")}`;
}

function isLikelyTaskDaemonPrompt(text: string): boolean {
  const normalized = text.toLowerCase();
  return (
    normalized.includes("based on a slack discussion in")
    && normalized.includes("tasks to create")
  );
}

function parseTaskDaemonCoordinatorHandoff(
  message: ChatMessage | null | undefined,
): TaskDaemonCoordinatorHandoff | null {
  if (!message || !Array.isArray(message.parts)) return null;

  for (const part of message.parts) {
    if (!isObject(part)) continue;
    const data = part.data;
    if (!isObject(data)) continue;

    const schemaVersion = normalizeOptionalString(data.schema_version);
    if (schemaVersion !== TASK_DAEMON_COORDINATOR_HANDOFF_SCHEMA_VERSION) continue;

    const batch = data.batch;
    if (!isObject(batch)) continue;
    return {
      schema_version: schemaVersion,
      batch,
    };
  }

  return null;
}

function renderHandoffPrompt(
  handoff: TaskDaemonCoordinatorHandoff,
  fallbackText: string,
): string {
  const batch = handoff.batch;
  const project = parseObjectField(batch, "project");
  const interpretation = parseObjectField(batch, "interpretation");
  const workflowSeed =
    interpretation != null ? parseObjectField(interpretation, "workflow_seed") : null;

  const projectKey = project != null ? normalizeOptionalString(project.project_key) : null;
  const repoAvailable = project != null ? parseOptionalBoolean(project.repo_available) : null;
  const repoPath = project != null ? normalizeOptionalString(project.repo_path) : null;
  const sourceLabel = normalizeOptionalString(batch.source_label);
  const messagesScanned = parseOptionalFiniteNumber(batch.messages_scanned);
  const summary =
    interpretation != null ? normalizeOptionalString(interpretation.executive_summary) : null;

  const objectives =
    interpretation != null
      ? parseStringArray(interpretation.current_objectives)
          .map((entry) => normalizeText(entry))
          .filter((entry) => entry.length > 0)
      : [];

  const decisions =
    interpretation != null
      ? parseObjectArray(interpretation.decisions_made)
          .map((decision) => {
            const decisionText = normalizeOptionalString(decision.decision);
            if (!decisionText) return null;
            const rationale = normalizeOptionalString(decision.rationale);
            const confidence = normalizeOptionalString(decision.confidence);
            const parts = [decisionText];
            if (rationale) parts.push(`rationale: ${rationale}`);
            if (confidence) parts.push(`confidence: ${confidence}`);
            return parts.join(" | ");
          })
          .filter((entry): entry is string => entry != null)
      : [];

  const openQuestions =
    interpretation != null
      ? parseObjectArray(interpretation.open_questions)
          .map((question) => {
            const text = normalizeOptionalString(question.question);
            if (!text) return null;
            const blocking = parseOptionalBoolean(question.blocking);
            const owner = normalizeOptionalString(question.suggested_owner);
            const parts = [text];
            if (blocking === true) parts.push("blocking");
            if (owner) parts.push(`owner: ${owner}`);
            return parts.join(" | ");
          })
          .filter((entry): entry is string => entry != null)
      : [];

  const risks =
    interpretation != null
      ? parseObjectArray(interpretation.risks)
          .map((risk) => {
            const riskText = normalizeOptionalString(risk.risk);
            if (!riskText) return null;
            const impact = normalizeOptionalString(risk.impact);
            const mitigation = normalizeOptionalString(risk.mitigation);
            const confidence = normalizeOptionalString(risk.confidence);
            const parts = [riskText];
            if (impact) parts.push(`impact: ${impact}`);
            if (mitigation) parts.push(`mitigation: ${mitigation}`);
            if (confidence) parts.push(`confidence: ${confidence}`);
            return parts.join(" | ");
          })
          .filter((entry): entry is string => entry != null)
      : [];

  const followUps =
    interpretation != null
      ? parseObjectArray(interpretation.follow_ups)
          .map((followUp) => {
            const prompt = normalizeOptionalString(followUp.prompt);
            if (!prompt) return null;
            const kind = normalizeOptionalString(followUp.kind);
            const urgency = normalizeOptionalString(followUp.urgency);
            const parts = [prompt];
            if (kind) parts.push(`kind: ${kind}`);
            if (urgency) parts.push(`urgency: ${urgency}`);
            return parts.join(" | ");
          })
          .filter((entry): entry is string => entry != null)
      : [];

  const workflowGoal = workflowSeed != null ? normalizeOptionalString(workflowSeed.goal) : null;

  const investigationNodes =
    workflowSeed != null
      ? parseObjectArray(workflowSeed.investigation_nodes)
          .map((node) => {
            const title = normalizeOptionalString(node.title);
            const key = normalizeOptionalString(node.key);
            const prompt = normalizeOptionalString(node.prompt);
            const goal = normalizeOptionalString(node.goal);
            const runCondition = normalizeOptionalString(node.when_to_run);
            const dependencies = parseStringArray(node.depends_on);
            const parts = [title || key];
            if (goal) parts.push(`goal: ${goal}`);
            if (runCondition) parts.push(`when: ${runCondition}`);
            if (dependencies.length > 0) parts.push(`depends_on: ${dependencies.join(", ")}`);
            if (prompt) parts.push(`prompt: ${prompt}`);
            return parts.filter((entry): entry is string => entry != null).join(" | ");
          })
          .filter((entry) => entry.length > 0)
      : [];

  const clarificationNodes =
    workflowSeed != null
      ? parseObjectArray(workflowSeed.clarification_nodes)
          .map((node) => {
            const question = normalizeOptionalString(node.question);
            if (!question) return null;
            const key = normalizeOptionalString(node.key);
            const owner = normalizeOptionalString(node.suggested_owner);
            const blocking = parseOptionalBoolean(node.blocking);
            const dependencies = parseStringArray(node.depends_on);
            const parts = [question];
            if (key) parts.push(`key: ${key}`);
            if (blocking === true) parts.push("blocking");
            if (owner) parts.push(`owner: ${owner}`);
            if (dependencies.length > 0) parts.push(`depends_on: ${dependencies.join(", ")}`);
            return parts.join(" | ");
          })
          .filter((entry): entry is string => entry != null)
      : [];

  const workflowFollowUps =
    workflowSeed != null
      ? parseObjectArray(workflowSeed.follow_up_nodes)
          .map((node) => {
            const prompt = normalizeOptionalString(node.prompt);
            if (!prompt) return null;
            const kind = normalizeOptionalString(node.kind);
            const urgency = normalizeOptionalString(node.urgency);
            const parts = [prompt];
            if (kind) parts.push(`kind: ${kind}`);
            if (urgency) parts.push(`urgency: ${urgency}`);
            return parts.join(" | ");
          })
          .filter((entry): entry is string => entry != null)
      : [];

  const derivedTasks = parseObjectArray(batch.derived_tasks)
    .map((task) => {
      const title = normalizeOptionalString(task.title);
      if (!title) return null;
      const key = normalizeOptionalString(task.key);
      const description = normalizeOptionalString(task.description);
      const priority = normalizeOptionalString(task.priority);
      const parts = [title];
      if (priority) parts.push(`priority: ${priority}`);
      if (key) parts.push(`key: ${key}`);
      if (description) parts.push(`description: ${description}`);
      const rendered = parts.join(" | ");
      return `${rendered}${formatTaskSources(task)}`;
    })
    .filter((entry): entry is string => entry != null);

  const lines: string[] = [];
  lines.push("Structured task-daemon handoff:");
  lines.push("Use this interpretation as the canonical input for workflow planning.");
  lines.push(`Handoff schema: ${handoff.schema_version}`);
  lines.push(`Project: ${projectKey || "unknown-project"}`);
  lines.push(`Source channel: ${sourceLabel || "unknown-source"}`);
  if (messagesScanned != null) {
    lines.push(`Messages scanned: ${Math.max(0, Math.floor(messagesScanned))}`);
  }
  if (repoAvailable != null) {
    lines.push(`Repository available: ${repoAvailable ? "yes" : "no"}`);
  }
  if (repoPath) {
    lines.push(`Repository path: ${repoPath}`);
  }
  lines.push("");

  if (summary) {
    lines.push(`Executive summary: ${truncateForPrompt(summary, 1_200)}`);
    lines.push("");
  }

  appendNumberedSection(lines, "Current objectives", objectives);
  appendNumberedSection(lines, "Decisions made", decisions);
  appendNumberedSection(lines, "Open questions", openQuestions);
  appendNumberedSection(lines, "Risks", risks);
  appendNumberedSection(lines, "Interpretation follow-ups", followUps);

  if (workflowGoal) {
    lines.push(`Workflow goal: ${truncateForPrompt(workflowGoal, 1_000)}`);
    lines.push("");
  }

  appendNumberedSection(lines, "Workflow investigation nodes", investigationNodes);
  appendNumberedSection(lines, "Workflow clarification nodes", clarificationNodes);
  appendNumberedSection(lines, "Workflow follow-up nodes", workflowFollowUps);
  appendNumberedSection(lines, "Derived tasks", derivedTasks);

  lines.push("Planning constraints:");
  lines.push("1. Prioritize workflow_seed and derived tasks over free-form phrasing.");
  lines.push("2. Treat interpretation as project-context understanding, not keyword matches.");
  if (repoAvailable === false) {
    lines.push("3. Repository is unavailable; favor clarification and follow-up workflows.");
  }

  const normalizedFallbackText = normalizeOptionalString(fallbackText);
  if (
    normalizedFallbackText
    && !isLikelyTaskDaemonPrompt(normalizedFallbackText)
  ) {
    lines.push("");
    lines.push("Additional operator message:");
    lines.push(truncateForPrompt(normalizedFallbackText, 1_600));
  }

  return lines.join("\n").slice(0, MAX_HANDOFF_PROMPT_CHARS);
}

export function buildPlannerUserTextFromTaskDaemonHandoff(
  message: ChatMessage | null | undefined,
  fallbackUserText: string,
): PlannerUserTextFromHandoff {
  const handoff = parseTaskDaemonCoordinatorHandoff(message);
  if (!handoff) {
    return {
      userText: fallbackUserText,
      structuredHandoff: false,
    };
  }

  const rendered = renderHandoffPrompt(handoff, fallbackUserText);
  if (rendered.trim().length === 0) {
    return {
      userText: fallbackUserText,
      structuredHandoff: true,
    };
  }

  return {
    userText: rendered,
    structuredHandoff: true,
  };
}
