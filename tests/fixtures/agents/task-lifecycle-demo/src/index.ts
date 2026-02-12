/// <reference path="./baml-runtime.d.ts" />
import type { SessionEmitter, SessionResult } from "./baml-runtime";
/**
 * Fixture: task-lifecycle-demo
 * ---------------------------
 * Didactic reference for the A2A DSL.
 *
 * What this demonstrates:
 * - Using __chat_register({ run }) so the entrypoint is run(ctx); ctx.emit for awaitInput/artifact.
 * - Suspending for user input with await ctx.emit.awaitInput(...); resuming on next message.
 * - Branching into success and failure terminal rails by returning { message } or { error }.
 * - Multiple sequential loops (no nesting): review loop, then sign-off loop.
 *
 * Lifecycle: path choice → Loop 1 (review) → Loop 2 (sign-off) → COMPLETED.
 * Example: "lifecycle-demo" -> "review-path" -> "approve" -> "confirm"
 */

const TRIGGER = "lifecycle-demo";

/** Loop 1: review. Returns approved (exit to next phase) or error (reject). */
async function runReviewLoop(emit: SessionEmitter): Promise<{ approved: true } | { error: string }> {
  for (;;) {
    const reviewDecision = messageText(
      await emit.awaitInput("Review decision: approve | reject | revise")
    ).toLowerCase();

    if (reviewDecision.includes("reject")) {
      return { error: "Rejected during review." };
    }

    if (reviewDecision.includes("approve")) {
      emit.message("Review approved. Proceeding to sign-off.");
      return { approved: true };
    }

    emit.message("Revision requested. Awaiting revision notes.");
    const revisionNotes = messageText(await emit.awaitInput("Provide revision notes"));
    emit.artifact(
      {
        name: "RevisionNotes",
        description: "Operator-provided revision guidance",
        parts: [{ mediaType: "text/plain", text: revisionNotes }],
      },
      false,
      true
    );
    emit.message(`Revision notes captured: ${revisionNotes}`);
  }
}

/** Loop 2: sign-off. Returns SessionResult (message or error). */
async function runSignOffLoop(emit: SessionEmitter): Promise<SessionResult> {
  for (;;) {
    const signOffDecision = messageText(
      await emit.awaitInput("Sign-off: confirm | request-changes | cancel")
    ).toLowerCase();

    if (signOffDecision.includes("cancel")) {
      return { error: "Sign-off cancelled." };
    }

    if (signOffDecision.includes("confirm")) {
      return { message: "Task completed after review and sign-off." };
    }

    emit.message("Change requested. Describe the change.");
    const changeText = messageText(await emit.awaitInput("Describe requested change"));
    emit.artifact(
      {
        name: "SignOffChangeRequest",
        description: "Requested change before sign-off",
        parts: [{ mediaType: "text/plain", text: changeText }],
      },
      false,
      true
    );
    emit.message(`Change request captured: ${changeText}`);
  }
}

__chat_register({
  run: async (ctx) => {
    const text = ctx.text || "unknown";
    if (!text.includes(TRIGGER)) {
      return { message: `Unknown trigger. Send a message containing "${TRIGGER}".` };
    }

    const emit = ctx.emit;
    emit.message("Task started.");
    emit.artifact(
      {
        name: "LifecycleArtifact",
        description: "Mid-session artifact",
        parts: [{ mediaType: "application/json", data: { phase: "working" } }],
      },
      false,
      true
    );

    const pathChoice = messageText(
      await emit.awaitInput("Choose path: fast-path | review-path | fail-now")
    ).toLowerCase();

    if (pathChoice.includes("fail-now")) {
      return { error: "Operator selected fail-now." };
    }

    if (pathChoice.includes("fast-path")) {
      emit.message("Fast path selected.");
      return { message: "Task completed via fast path." };
    }

    emit.message("Review path selected.");
    emit.artifact(
      {
        name: "ReviewPacket",
        description: "Draft output pending review",
        parts: [{ mediaType: "application/json", data: { review: "required" } }],
      },
      false,
      true
    );

    const reviewOut = await runReviewLoop(emit);
    if ("error" in reviewOut) return reviewOut;

    emit.artifact(
      {
        name: "SignOffPacket",
        description: "Ready for final sign-off",
        parts: [{ mediaType: "application/json", data: { phase: "sign-off" } }],
      },
      false,
      true
    );

    return runSignOffLoop(emit);
  },
});
