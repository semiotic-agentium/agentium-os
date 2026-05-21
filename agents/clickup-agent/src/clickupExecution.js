export const MAX_REACT_STEPS = 8;
const PRIOR_RESULTS_MAX_CHARS = 6000;
export const PKG_CLICKUP_EXECUTE = "clickup-execute";
export const PKG_CLICKUP_FORMAT = "clickup-format";
export function isJsonObject(v) {
    return v !== null && typeof v === "object" && !Array.isArray(v);
}
export function isNeedClarification(v) {
    return (isJsonObject(v) &&
        typeof v.question === "string" &&
        v.question.trim().length > 0 &&
        !("message" in v) &&
        !("intent" in v) &&
        !("reason" in v) &&
        !("steps" in v));
}
export function isNotRelevant(v) {
    return isJsonObject(v) && typeof v.reason === "string" && !("question" in v) && !("intent" in v);
}
export function isClickUpIntent(v) {
    return (isJsonObject(v) &&
        typeof v.intent === "string" &&
        v.intent.trim().length > 0 &&
        typeof v.operation_kind === "string" &&
        !("question" in v) &&
        !("reason" in v));
}
function slugGoal(goal) {
    return goal.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 48) || "goal";
}
function stringifyUnknown(value, max) {
    try {
        const s = JSON.stringify(value, null, 2);
        return s.length > max ? `${s.slice(0, max)}\n…` : s;
    }
    catch {
        const s = String(value);
        return s.length > max ? `${s.slice(0, max)}…` : s;
    }
}
function extractToolLikePayload(v) {
    if (isJsonObject(v) && (Array.isArray(v.tasks) || Array.isArray(v.items)))
        return v;
    if (isJsonObject(v) &&
        isJsonObject(v.output) &&
        (Array.isArray(v.output.tasks) || Array.isArray(v.output.items))) {
        return v.output;
    }
    return null;
}
function executorStepToPriorContextText(step) {
    if (step == null)
        return "";
    if (isFinalResponse(step)) {
        return `[final_response]\n${step.message}`.trim();
    }
    const toolLike = extractToolLikePayload(step);
    if (toolLike) {
        return `[tool_result]\n${formatListLikeToolPayload(toolLike)}`.trim();
    }
    if (isJsonObject(step) && typeof step.message === "string" && step.message.trim()) {
        return `[message]\n${step.message.trim()}`;
    }
    return `[raw]\n${stringifyUnknown(step, 3500)}`;
}
function collectStepResultsForPriorContext(steps) {
    const parts = [];
    for (const step of steps) {
        const block = executorStepToPriorContextText(step);
        if (block)
            parts.push(block);
    }
    const joined = parts.join("\n\n---\n\n");
    if (joined.trim())
        return joined.slice(0, PRIOR_RESULTS_MAX_CHARS);
    return stringifyUnknown(steps.slice(-5), PRIOR_RESULTS_MAX_CHARS);
}
export function textReply(text) {
    const parts = [{ type: "text", text }];
    return { parts, citations: [] };
}
function finalResponseToStructured(fr) {
    const msg = fr.message.trim() || "Done.";
    const parts = [{ type: "text", text: msg }];
    const sj = typeof fr.structured_json === "string" ? fr.structured_json.trim() : "";
    if (sj) {
        parts.push({ type: "data", data: sj, media_type: "application/json" });
    }
    const citations = Array.isArray(fr.citations)
        ? fr.citations.filter((c) => typeof c === "string")
        : [];
    return { parts, citations };
}
function formatLineFromClickUpItem(entry) {
    if (!isJsonObject(entry))
        return "";
    const kind = typeof entry.kind === "string" ? entry.kind : "";
    const name = typeof entry.name === "string" ? entry.name : "";
    const id = typeof entry.id === "string" ? entry.id : "";
    if (!name && !id)
        return "";
    return `• [${kind}] ${name} (id: ${id})`;
}
function formatLineFromClickUpTaskSummary(entry) {
    if (!isJsonObject(entry))
        return "";
    const name = typeof entry.name === "string" ? entry.name : "Unnamed task";
    const status = typeof entry.status === "string" ? entry.status : "unknown";
    const url = typeof entry.url === "string" ? entry.url : "";
    return `• ${name} [${status}]${url ? ` — ${url}` : ""}`;
}
function formatListLikeToolPayload(output) {
    const msg = output.message;
    let response = typeof msg === "string" ? msg : "Done.";
    const items = output.items;
    if (Array.isArray(items) && items.length > 0) {
        response += "\n\n" + items.map(formatLineFromClickUpItem).filter((s) => s.length > 0).join("\n");
    }
    const tasks = output.tasks;
    if (Array.isArray(tasks) && tasks.length > 0) {
        response += "\n\n" + tasks.map(formatLineFromClickUpTaskSummary).filter((s) => s.length > 0).join("\n");
    }
    return response;
}
function isFinalResponse(v) {
    if (!isJsonObject(v))
        return false;
    if (typeof v.message !== "string")
        return false;
    return !("tasks" in v || "items" in v || "steps" in v || "action" in v || "intent" in v);
}
export function extractFinalMessage(steps) {
    for (const step of [...steps].reverse()) {
        if (isFinalResponse(step))
            return finalResponseToStructured(step);
        const toolLike = extractToolLikePayload(step);
        if (toolLike)
            return textReply(formatListLikeToolPayload(toolLike));
        if (isJsonObject(step) && typeof step.message === "string" && step.message.trim()) {
            return textReply(step.message.trim());
        }
    }
    if (steps.length > 0) {
        const raw = stringifyUnknown(steps[steps.length - 1], 4000);
        if (raw.trim())
            return textReply(`ClickUp session produced:\n${raw}`);
    }
    return textReply("ClickUp returned no usable response for this request.");
}
/** Flatten SessionResult into a short dispatch ack detail string. */
export function sessionResultDetail(result, maxLen = 800) {
    if ("error" in result) {
        return result.error.length > maxLen ? `${result.error.slice(0, maxLen)}…` : result.error;
    }
    const msg = result.message;
    if (typeof msg === "string") {
        return msg.length > maxLen ? `${msg.slice(0, maxLen)}…` : msg;
    }
    const textPart = msg.parts?.find((p) => p.type === "text" && typeof p.text === "string");
    const text = textPart && "text" in textPart ? String(textPart.text) : "ClickUp lifecycle ingress completed.";
    return text.length > maxLen ? `${text.slice(0, maxLen)}…` : text;
}
function filterPlanCitations(plan) {
    const raw = plan.citations;
    if (!Array.isArray(raw))
        return [];
    return raw.filter((c) => typeof c === "string" && c.trim().length > 0);
}
/** Coordination-only: verify the runtime value matches ClickUpStructuredPlan after PlanClickUpWork. */
export function parseClickUpStructuredPlanFromPlanning(v) {
    if (!isJsonObject(v))
        return null;
    if (typeof v.intent_description !== "string" || typeof v.objective !== "string")
        return null;
    if (!Array.isArray(v.plan_steps))
        return null;
    const c = v.citations;
    if (c != null && !Array.isArray(c))
        return null;
    return v;
}
/** @deprecated Use parseClickUpStructuredPlanFromPlanning */
export const parseStandardStructuredPlanFromPlanning = parseClickUpStructuredPlanFromPlanning;
export function validateClickUpPlanForExecution(plan) {
    const raw = plan.plan_steps;
    if (raw.length === 0)
        return "plan_steps is empty";
    const steps = [];
    for (let i = 0; i < raw.length; i++) {
        const s = raw[i];
        if (!isJsonObject(s))
            return `plan_steps[${i}] is not an object`;
        if (typeof s.sub_message !== "string" || !s.sub_message.trim()) {
            return `plan_steps[${i}].sub_message must be a non-empty string`;
        }
        const pkgRaw = s.agent_package;
        const instRaw = s.agent_instance_id;
        if (typeof pkgRaw !== "string" || typeof instRaw !== "string") {
            return `plan_steps[${i}] missing agent_package or agent_instance_id`;
        }
        const pkg = pkgRaw.trim().toLowerCase();
        if (pkg !== PKG_CLICKUP_EXECUTE && pkg !== PKG_CLICKUP_FORMAT) {
            return `plan_steps[${i}] has invalid agent_package "${pkgRaw}" (expected clickup-execute or clickup-format)`;
        }
        steps.push({
            agent_package: pkgRaw.trim(),
            agent_instance_id: instRaw.trim() || "default",
            sub_message: s.sub_message,
        });
    }
    let executeCount = 0;
    let formatCount = 0;
    for (const st of steps) {
        const p = st.agent_package.trim().toLowerCase();
        if (p === PKG_CLICKUP_EXECUTE)
            executeCount++;
        if (p === PKG_CLICKUP_FORMAT)
            formatCount++;
    }
    if (executeCount < 1)
        return "plan must include at least one clickup-execute step";
    if (formatCount !== 1)
        return "plan must include exactly one clickup-format step";
    const lastPkg = steps[steps.length - 1].agent_package.trim().toLowerCase();
    if (lastPkg !== PKG_CLICKUP_FORMAT)
        return "last plan step must be clickup-format";
    return steps;
}
export async function executeClickUpPlan(_ctx, structured, validatedIntent, operationKind, steps) {
    const goal = structured.objective.trim() || validatedIntent;
    const intentSlug = slugGoal(structured.intent_description || goal);
    // Dispatch ingress has no conversational task scope; skip planning session metadata there.
    const executionSession = _ctx != null && typeof openA2aExecutionSession === "function"
        ? await openA2aExecutionSession("clickup-" + Date.now().toString())
        : null;
    const intentId = "intent-clickup-" + intentSlug;
    const citations = filterPlanCitations(structured);
    const intentPhase = executionSession
        ? await executionSession.submitIntent({
            intentId,
            description: structured.intent_description || goal,
            ...(citations.length > 0 ? { citations } : {}),
        })
        : null;
    const executable = intentPhase
        ? await intentPhase.submitPlan({
            intentId,
            planId: "plan-clickup-" + intentSlug,
            steps: steps.map((s, i) => ({
                stepId: "step-" + i,
                description: s.sub_message,
                order: i,
                dependsOn: i > 0 ? ["step-" + (i - 1)] : [],
            })),
        })
        : null;
    const allStepOutputsNested = [];
    let priorResultsText = null;
    try {
        for (let i = 0; i < steps.length; i++) {
            const step = steps[i];
            const stepId = "step-" + i;
            const pkg = step.agent_package.trim().toLowerCase();
            if (executable)
                await executable.startStep?.(stepId);
            if (pkg === PKG_CLICKUP_EXECUTE) {
                const run = await runGeneratedStepExecutor("ChooseClickUpAction", {
                    goal,
                    step_description: step.sub_message,
                    operation_kind: operationKind,
                    prior_results: priorResultsText,
                }, { max_steps: MAX_REACT_STEPS });
                if (run.outcome !== "completed") {
                    if (run.outcome === "agent_correctable") {
                        throw new Error(`[${run.recovery.code}] ${run.recovery.mistake}`);
                    }
                    throw new Error(run.message);
                }
                allStepOutputsNested.push(run.steps);
                priorResultsText = collectStepResultsForPriorContext(run.steps);
                if (executable)
                    await executable.completeStep?.(stepId);
            }
            else if (pkg === PKG_CLICKUP_FORMAT) {
                const finalMessage = extractFinalMessage(allStepOutputsNested.flat());
                if (executable)
                    await executable.completeStep?.(stepId);
                if (executable)
                    await executable.finish?.();
                return { message: finalMessage };
            }
            else {
                if (executable)
                    await executable.completeStep?.(stepId);
            }
        }
        if (executable)
            await executable.finish?.();
        return { message: textReply("ClickUp plan completed without a format step.") };
    }
    catch (e) {
        const errMsg = e instanceof Error ? e.message : String(e);
        try {
            if (executable)
                await executable.abort?.(errMsg);
        }
        catch (_) { /* best-effort */ }
        return { error: `ClickUp agent error: ${errMsg}` };
    }
}
// ── Host source-records ingress (host.source-records.v1 / clickup) ───────────
const INGRESS_AGENT_NAME = "clickup-agent";
const RAW_SOURCE_SCHEMA_VERSION = "host.source-records.v1";
const RAW_SOURCE_ROUTING_KEY = "event:intake";
function normalizeOptionalString(value) {
    if (typeof value !== "string")
        return null;
    const trimmed = value.trim();
    return trimmed.length > 0 ? trimmed : null;
}
export function parseClickupSourceRecordsBatch(value) {
    if (!isJsonObject(value))
        return null;
    const schemaVersion = normalizeOptionalString(value.schema_version);
    if (!schemaVersion)
        return null;
    const emitted = value.emitted_at_unix;
    if (typeof emitted !== "number" || !Number.isFinite(emitted))
        return null;
    const source = value.source;
    if (!isJsonObject(source))
        return null;
    const sourceKind = normalizeOptionalString(source.source_kind);
    const sourceKey = normalizeOptionalString(source.source_key);
    const sourceLabel = normalizeOptionalString(source.source_label);
    if (!sourceKind || !sourceKey || !sourceLabel)
        return null;
    if (!Array.isArray(value.records))
        return null;
    const records = [];
    for (const row of value.records) {
        if (!isJsonObject(row))
            return null;
        const recordKind = normalizeOptionalString(row.record_kind);
        const key = normalizeOptionalString(row.key);
        const title = normalizeOptionalString(row.title);
        if (!recordKind || !key || !title)
            return null;
        records.push({
            record_kind: recordKind,
            key,
            title,
            description: typeof row.description === "string" ? row.description : "",
            priority: typeof row.priority === "string" ? row.priority : "",
            sources: Array.isArray(row.sources) ? row.sources : [],
        });
    }
    let project;
    if (isJsonObject(value.project)) {
        const projectKey = normalizeOptionalString(value.project.project_key);
        if (projectKey) {
            project = {
                project_key: projectKey,
                repo_available: value.project.repo_available === true,
                repo_path: typeof value.project.repo_path === "string" ? value.project.repo_path : undefined,
            };
        }
    }
    return {
        schema_version: schemaVersion,
        emitted_at_unix: emitted,
        source: {
            source_kind: sourceKind,
            source_key: sourceKey,
            source_label: sourceLabel,
        },
        project,
        records,
    };
}
function groupClickupLifecycleUnits(records) {
    const groups = new Map();
    for (const record of records) {
        const key = normalizeOptionalString(record.key);
        if (!key)
            continue;
        const row = record;
        const existing = groups.get(key);
        if (existing) {
            existing.push(row);
        }
        else {
            groups.set(key, [row]);
        }
    }
    return Array.from(groups.entries()).map(([unitKey, unitRecords]) => ({
        unitKey,
        records: unitRecords,
    }));
}
async function processClickupLifecycleUnit() {
    const intentResult = await InferClickUpIntent({});
    if (isNotRelevant(intentResult)) {
        return { ok: true, detail: "skipped:not_relevant" };
    }
    if (isNeedClarification(intentResult)) {
        return {
            ok: false,
            detail: `${INGRESS_AGENT_NAME} cannot clarify during dispatch: ${intentResult.question}`,
        };
    }
    if (!isClickUpIntent(intentResult)) {
        return {
            ok: false,
            detail: `${INGRESS_AGENT_NAME} expected ClickUpIntent from InferClickUpIntent for lifecycle ingress.`,
        };
    }
    const planResult = await PlanClickUpWork({
        intent: intentResult.intent,
        operation_kind: intentResult.operation_kind,
    });
    const structured = parseClickUpStructuredPlanFromPlanning(planResult);
    if (!structured) {
        return {
            ok: false,
            detail: `${INGRESS_AGENT_NAME} planning failed: PlanClickUpWork did not return ClickUpStructuredPlan.`,
        };
    }
    const stepsOrErr = validateClickUpPlanForExecution(structured);
    if (typeof stepsOrErr === "string") {
        return {
            ok: false,
            detail: `${INGRESS_AGENT_NAME} invalid plan for ingress: ${stepsOrErr}`,
        };
    }
    const result = await executeClickUpPlan(null, structured, intentResult.intent, intentResult.operation_kind, stepsOrErr);
    if ("error" in result) {
        return { ok: false, detail: result.error };
    }
    return { ok: true, detail: sessionResultDetail(result) };
}
async function handleClickupLifecycleBatch(ctx) {
    const batch = ctx.batch;
    const units = groupClickupLifecycleUnits(batch.records);
    if (units.length === 0) {
        return { accepted: true, detail: "No lifecycle units in batch." };
    }
    let unitsProcessed = 0;
    const unitSummaries = [];
    for (const unit of units) {
        let unitOutcome;
        try {
            unitOutcome = await ctx.withTask({ unitKey: unit.unitKey, records: unit.records }, async () => processClickupLifecycleUnit());
        }
        catch (error) {
            const reason = error instanceof Error ? error.message : String(error);
            return {
                accepted: false,
                detail: `${INGRESS_AGENT_NAME} failed on unit ${unit.unitKey}: ${reason}`,
            };
        }
        if (!unitOutcome.ok) {
            return { accepted: false, detail: unitOutcome.detail };
        }
        if (unitOutcome.detail !== "skipped:not_relevant") {
            unitsProcessed += 1;
            unitSummaries.push(`${unit.unitKey}=${unitOutcome.detail}`);
        }
    }
    return {
        accepted: true,
        detail: `Processed ClickUp lifecycle ingress: ${unitsProcessed}/${units.length} unit(s) ` +
            `from ${batch.records.length} record(s)` +
            (unitSummaries.length > 0 ? ` (${unitSummaries.join("; ")})` : ""),
    };
}
export async function onClickupSourceDispatch(ctx) {
    const request = ctx.request;
    const messageType = normalizeOptionalString(request.message_type);
    if (messageType !== RAW_SOURCE_SCHEMA_VERSION) {
        return {
            accepted: false,
            detail: `${INGRESS_AGENT_NAME} expected message_type ${RAW_SOURCE_SCHEMA_VERSION}, ` +
                `got ${messageType ?? "missing"}.`,
        };
    }
    const routingKey = normalizeOptionalString(request.routing_key);
    if (routingKey !== RAW_SOURCE_ROUTING_KEY) {
        return {
            accepted: false,
            detail: `${INGRESS_AGENT_NAME} expected routing_key ${RAW_SOURCE_ROUTING_KEY}, ` +
                `got ${routingKey ?? "missing"}.`,
        };
    }
    if (request.messages.length !== 1) {
        return {
            accepted: false,
            detail: `${INGRESS_AGENT_NAME} expected exactly one dispatch message, ` +
                `got ${request.messages.length}.`,
        };
    }
    const batch = ctx.batch;
    if (!batch || batch.schema_version !== RAW_SOURCE_SCHEMA_VERSION) {
        return {
            accepted: false,
            detail: `${INGRESS_AGENT_NAME} expected ${RAW_SOURCE_SCHEMA_VERSION} ClickupSourceRecordsBatch ` +
                `in dispatch.messages[0].`,
        };
    }
    if (batch.records.length === 0) {
        return { accepted: true, detail: "No lifecycle records in batch." };
    }
    try {
        return await handleClickupLifecycleBatch(ctx);
    }
    catch (error) {
        const reason = error instanceof Error ? error.message : String(error);
        return {
            accepted: false,
            detail: `${INGRESS_AGENT_NAME} failed: ${reason}`,
        };
    }
}
