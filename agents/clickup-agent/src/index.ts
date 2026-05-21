/// <reference path="./baml-runtime.d.ts" />
import type { ClickUpIntent, RunContext, SessionResult } from "./baml-runtime";
import {
  executeClickUpPlan,
  isClickUpIntent,
  isNeedClarification,
  isNotRelevant,
  onClickupSourceDispatch,
  parseClickUpStructuredPlanFromPlanning,
  textReply,
  validateClickUpPlanForExecution,
} from "./clickupExecution";

__chat_register({
  run: async (ctx: RunContext): Promise<SessionResult> => {
    const originalText = typeof ctx.text === "string" && ctx.text.length > 0 ? ctx.text : "unknown";
    let text = originalText;

    let resolvedIntent: ClickUpIntent | null = null;
    while (true) {
      const intentResult = await InferClickUpIntent({});

      if (isClickUpIntent(intentResult)) {
        resolvedIntent = intentResult;
        break;
      }
      if (isNotRelevant(intentResult)) {
        return {
          message: textReply(`This doesn't look like a ClickUp request — ${intentResult.reason}`),
        };
      }
      if (isNeedClarification(intentResult)) {
        const reply = await ctx.emit.awaitInput(intentResult.question);
        const clarifiedText = messageText(reply).trim();
        if (clarifiedText) text = clarifiedText;
        continue;
      }
      return {
        error:
          "InferClickUpIntent returned an unexpected shape — expected ClickUpIntent, NeedClarification, or NotRelevant.",
      };
    }
    if (!resolvedIntent) return { error: "Could not determine a valid ClickUp intent." };

    const planResult = await PlanClickUpWork({
      intent: resolvedIntent.intent,
      operation_kind: resolvedIntent.operation_kind,
    });
    const structured = parseClickUpStructuredPlanFromPlanning(planResult);
    if (!structured) {
      return {
        error:
          "Planning failed: PlanClickUpWork did not return a valid ClickUpStructuredPlan shape. Try rephrasing your request.",
      };
    }
    const stepsOrErr = validateClickUpPlanForExecution(structured);
    if (typeof stepsOrErr === "string") {
      return {
        error: `Planning output did not satisfy the ClickUp execution contract: ${stepsOrErr}`,
      };
    }

    return executeClickUpPlan(
      ctx,
      structured,
      resolvedIntent.intent,
      resolvedIntent.operation_kind,
      stepsOrErr,
    );
  },

  onDispatch: onClickupSourceDispatch,
});
