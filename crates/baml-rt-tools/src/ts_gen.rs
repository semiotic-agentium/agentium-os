use baml_rt_core::{BamlRtError, Result};
use genco::{lang::js, prelude::*};

use crate::tools::ToolFunctionMetadata;

pub fn render_tool_typescript(tools: &[ToolFunctionMetadata]) -> Result<String> {
    let mut tokens: js::Tokens = quote!(
        // TypeScript declarations for A2A task-only runtime API
        // This file is auto-generated - do not edit manually
    );
    tokens.line();

    let a2a_fsm_ts = r#"
/**
 * A2A task-only FSM API (message-first).
 *
 * This is the canonical interface for A2A task flows in TypeScript.
 * It intentionally models legal task/session transitions in the type system
 * so invalid transition wiring fails at compile-time.
 */
export type A2aTaskState =
    | "TASK_STATE_SUBMITTED"
    | "TASK_STATE_WORKING"
    | "TASK_STATE_INPUT_REQUIRED"
    | "TASK_STATE_AUTH_REQUIRED"
    | "TASK_STATE_COMPLETED"
    | "TASK_STATE_FAILED"
    | "TASK_STATE_CANCELED"
    | "TASK_STATE_REJECTED";

export type A2aNonTerminalTaskState =
    | "TASK_STATE_SUBMITTED"
    | "TASK_STATE_WORKING"
    | "TASK_STATE_INPUT_REQUIRED"
    | "TASK_STATE_AUTH_REQUIRED";

export type A2aTerminalTaskState =
    | "TASK_STATE_COMPLETED"
    | "TASK_STATE_FAILED"
    | "TASK_STATE_CANCELED"
    | "TASK_STATE_REJECTED";

export type A2aNextStates<S extends A2aTaskState> =
    S extends "TASK_STATE_SUBMITTED"
        ? "TASK_STATE_WORKING" | "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED" | "TASK_STATE_REJECTED" | "TASK_STATE_INPUT_REQUIRED" | "TASK_STATE_AUTH_REQUIRED"
    : S extends "TASK_STATE_WORKING"
        ? "TASK_STATE_INPUT_REQUIRED" | "TASK_STATE_AUTH_REQUIRED" | "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED" | "TASK_STATE_REJECTED"
    : S extends "TASK_STATE_INPUT_REQUIRED"
        ? "TASK_STATE_WORKING" | "TASK_STATE_CANCELED" | "TASK_STATE_REJECTED"
    : S extends "TASK_STATE_AUTH_REQUIRED"
        ? "TASK_STATE_WORKING" | "TASK_STATE_CANCELED" | "TASK_STATE_REJECTED"
    : never;

export interface A2aTaskContext<S extends A2aTaskState> {
    taskId: string;
    contextId?: string;
    state: S;
}

export interface A2aTaskView<S extends A2aTaskState = A2aTaskState> extends A2aTaskContext<S> {}

export interface A2aActiveTask<S extends A2aNonTerminalTaskState> extends A2aTaskView<S> {
    onStatus<N extends A2aNextStates<S>>(expected: N, handler: (task: A2aActiveTask<N>) => Promise<void> | void): A2aActiveTask<S>;
    cancel(): Promise<A2aTerminalTask<"TASK_STATE_CANCELED">>;
}

export interface A2aTerminalTask<S extends A2aTerminalTaskState> extends A2aTaskView<S> {}

export interface A2aStateDispatcher<S extends A2aNonTerminalTaskState> {
    on<N extends A2aNextStates<S>>(state: N, handler: (ctx: A2aTaskContext<N>) => Promise<void> | void): A2aStateDispatcher<S>;
}

export type JsonPrimitive = string | number | boolean | null;
export type JsonObject = { [key: string]: JsonValue };
export type JsonArray = JsonValue[];
export type JsonValue = JsonPrimitive | JsonObject | JsonArray;

/** A chunk emitted via __chat_yield. Must be a JSON-serializable object following A2A wire format. */
export type YieldChunk =
  | { message: { parts: Part[]; role?: string; [key: string]: JsonValue | undefined }; [key: string]: JsonValue | undefined }
  | { task: { id?: string; contextId?: string; status?: { state?: A2aTaskState; [key: string]: JsonValue | undefined }; [key: string]: JsonValue | undefined }; [key: string]: JsonValue | undefined }
  | { statusUpdate: JsonObject; [key: string]: JsonValue | undefined }
  | { artifactUpdate: JsonObject; [key: string]: JsonValue | undefined }
  | { final: boolean; [key: string]: JsonValue | undefined }
  | JsonObject;

export type A2aMessageEvent<S extends A2aTaskState = A2aTaskState> =
    | { kind: "assistantMessage"; text: string; task: A2aTaskView<S> }
    | { kind: "statusChanged"; from?: A2aTaskState; to: S; task: A2aTaskView<S> }
    | { kind: "artifactPublished"; task: A2aTaskView<S>; artifact: JsonValue; append?: boolean; lastChunk?: boolean }
    | { kind: "completed"; task: A2aTerminalTask<Extract<S, A2aTerminalTaskState>> }
    | { kind: "failed"; task: A2aTerminalTask<Extract<S, A2aTerminalTaskState>>; error: ToolFailure };

/**
 * Session typestate rails:
 * - AwaitingInput: only send/abort are legal
 * - Streaming: event consumption + finish/abort
 * - Closed: terminal state, no further operations
 */
export interface A2aSessionAwaitingInput<I> {
    sessionId: string;
    send(input: I): Promise<A2aSessionStreaming<"TASK_STATE_SUBMITTED">>;
    abort(reason?: string): Promise<A2aSessionClosed>;
}

export interface A2aSessionStreaming<S extends A2aTaskState> {
    sessionId: string;
    events(): AsyncIterable<A2aMessageEvent<S | A2aNextStates<S>>>;
    dispatch(task: A2aActiveTask<Extract<S, A2aNonTerminalTaskState>>): A2aStateDispatcher<Extract<S, A2aNonTerminalTaskState>>;
    finish(): Promise<A2aSessionClosed>;
    abort(reason?: string): Promise<A2aSessionClosed>;
}

export interface A2aSessionClosed {
    sessionId: string;
    closed: true;
}

/**
 * Intent/Plan protocol rails (breaking contract):
 * 1) submitIntent(...)
 * 2) submitPlan(...)
 * 3) execute and complete steps with strict evidence references
 */
export interface IntentSubmission {
    intentId: string;
    description: string;
    derivedFromMessageIds: string[];
    supersession?: "replaced" | "refined";
}

export interface PlanStepSubmission {
    stepId: string;
    description: string;
    order: number;
    dependsOn?: string[];
}

export interface PlanSubmission {
    intentId: string;
    planId: string;
    steps: PlanStepSubmission[];
    supersession?: "replaced" | "refined";
}

export interface A2aExecutionSessionAwaitIntent {
    sessionId: string;
    submitIntent(intent: IntentSubmission): Promise<A2aExecutionSessionAwaitPlan>;
    abort(reason?: string): Promise<A2aSessionClosed>;
}

export interface A2aExecutionSessionAwaitPlan {
    sessionId: string;
    submitPlan(plan: PlanSubmission): Promise<A2aExecutionSessionExecutable>;
    abort(reason?: string): Promise<A2aSessionClosed>;
}

export interface A2aExecutionSessionExecutable {
    sessionId: string;
    startStep(stepId: string, evidenceText: string): Promise<void>;
    completeStep(stepId: string, evidenceText: string): Promise<void>;
    finish(): Promise<A2aSessionClosed>;
    abort(reason?: string): Promise<A2aSessionClosed>;
}

/**
 * Bootstrap-generated: handler types. Incoming message (parts only; IDs/context are host-managed).
 * Session lifecycle: host invokes onChatMessage(message); agent uses session(message).run(...) to run work and emit outcomes.
 */
export interface Part { text?: string; data?: JsonValue; [key: string]: JsonValue | undefined; }
export interface Message {
  parts: Part[];
  /** First text part. Present on messages from awaitInput(); for the initial message use session(message).text(). */
  text?(): string;
}
export type ChatMessage = Message;

/**
 * Result shape for session.run() callback. Return { message } on success (runtime emits message and completed);
 * return { error } on failure (runtime emits failed with that error). No need to call emit helpers yourself.
 */
export type SessionResult = { message: string } | { error: string };

/**
 * Emitter passed into run(emit => ...) for intermediate emissions (working message, artifact, status).
 * Use when you need to stream artifacts or status before returning the final SessionResult.
 */
export interface SessionEmitter {
  /** Emit a working message (task state remains WORKING). */
  message(text: string): void;
  /** Emit an artifact chunk (append/lastChunk optional). */
  artifact(artifact: JsonValue, append?: boolean, lastChunk?: boolean): void;
  /** Emit a status transition (e.g. TASK_STATE_WORKING). */
  statusChanged(to: A2aTaskState): void;
  /**
   * Suspend this flow until the next message for the same task/context arrives.
   * Runtime emits TASK_STATE_INPUT_REQUIRED before suspension. Optional prompt is attached to that status.
   */
  awaitInput(prompt?: string): Promise<ChatMessage>;
}

/**
 * Fluent session builder. Entrypoint for agent logic: session(message).run(async () => ...).
 * - .text() returns the first text part of the message (no manual extraction).
 * - .run(fn) runs fn(); if it returns { message }, runtime emits that and completed; if { error }, runtime emits failed.
 * - .run(emit => fn(emit)) receives an emitter for intermediate message/artifact/status and await-input suspension.
 * - .onCompleted / .onFailed are optional side-effect callbacks; emission is always done by the runtime.
 */
export interface SessionBuilder {
  /** First text part of the message; use for BAML/tool args. Defaults to "" if missing. */
  text(): string;
  /** Optional: called with the success message before the runtime emits completed. */
  onCompleted(fn: (message: string) => void): SessionBuilder;
  /** Optional: called with the error string before the runtime emits failed. */
  onFailed(fn: (error: string) => void): SessionBuilder;
  /**
   * Run async work. Return { message } to succeed (runtime emits message + completed);
   * return { error } to fail (runtime emits failed). Rejected promise is treated as { error: err.message }.
   * Overload: run(emit => ...) receives SessionEmitter for intermediate emissions.
   */
  run(fn: (emit: SessionEmitter) => Promise<SessionResult>): Promise<void>;
  run(fn: () => Promise<SessionResult>): Promise<void>;
}

/**
 * Context passed to the run entrypoint when using __chat_register({ run }).
 * The real entrypoint is run(ctx): you receive text, message, and emit; return SessionResult.
 */
export interface RunContext {
  /** First text part of the message (same as session(message).text()). */
  text: string;
  /** Inbound message (parts, optional .text() from awaitInput). */
  message: ChatMessage;
  /** Emitter for working message, artifact, awaitInput; use when you need to stream or suspend. */
  emit: SessionEmitter;
}

/**
 * Host-to-agent dispatch request. Delivered by the host when an external event
 * matches this agent's subscriptions. Fields mirror the Rust AgentDispatchRequest.
 */
export interface HostDispatchRequest {
  routing_key: string;
  message_type: string;
  messages: JsonValue[];
  context_id?: string;
  task_id?: string;
  message_id?: string;
  /** Structured transport metadata (source, schema version, content type). Use `messages` for arbitrary event payloads. */
  metadata?: JsonObject;
}

/**
 * Acknowledgement returned by an agent's onDispatch handler.
 */
export interface HostDispatchAck {
  accepted: boolean;
  detail?: string;
}

/** Agent contract: register this; host invokes onChatMessage per message. */
export interface BamlAgent {
  /** Optional: run(ctx) is the entrypoint; runtime wraps it into onChatMessage. Prefer this over onChatMessage. */
  run?(ctx: RunContext): Promise<SessionResult>;
  /** Optional: raw handler when run is not used. */
  onChatMessage?(message: ChatMessage): Promise<void>;
  /** Optional: handle host-delivered events matched by this agent's subscriptions. */
  onDispatch?(request: HostDispatchRequest): Promise<HostDispatchAck>;
  tools?: Record<string, (args: JsonObject) => Promise<JsonValue>>;
}

declare global {
  /** Minimal console interface matching the QuickJS sandbox polyfill (log, info, warn, error, debug). */
  var console: {
    log(...args: unknown[]): void;
    info(...args: unknown[]): void;
    warn(...args: unknown[]): void;
    error(...args: unknown[]): void;
    debug(...args: unknown[]): void;
  };

  /**
   * Global alias for incoming chat messages in agent entrypoints.
   * This keeps bootstrap/index.ts ergonomic without local type shims.
   */
  type ChatMessage = Message;
  /**
   * First text part of a message. Use for any ChatMessage (e.g. from awaitInput).
   * For the initial message you can also use session(message).text().
   */
  function messageText(message: ChatMessage | null | undefined): string;
  /**
   * Start a session for this message. Use the returned builder to get .text(), then .run(async () => ...).
   * Legal transitions and emission are handled by the runtime; you only return success or error from run().
   */
  function session(message: ChatMessage | null | undefined): SessionBuilder;
  function __chat_register(agent: BamlAgent): void;
  /**
   * Extract the first payload from a host dispatch request.
   * Returns the first element of request.messages cast to T, or null if absent.
   */
  function extractDispatchEvent<T = JsonValue>(request: HostDispatchRequest | null | undefined): T | null;
  /** Emit a stream chunk following A2A wire format. */
  function __chat_yield(chunk: YieldChunk): void;
  function openA2aTaskSession<I = Record<string, unknown>>(token: string): Promise<A2aSessionAwaitingInput<I>>;
  function openA2aExecutionSession(token: string): Promise<A2aExecutionSessionAwaitIntent>;
}
"#;
    for line in a2a_fsm_ts.lines() {
        quote_in!(tokens => $(line));
        tokens.push();
    }
    tokens.line();

    quote_in!(tokens =>
        export type ToolFailureKind =
            | "InvalidInput"
            | "ExecutionFailed"
            | "NotAuthorized"
            | "RateLimited"
            | "Cancelled"
            | "Unknown";

        export interface ToolFailure {
            kind: ToolFailureKind;
            message: string;
            retryable: boolean;
        }
    );
    tokens.line();
    // Dispatch types are agent-agnostic; tools parameter reserved for future tool-specific codegen
    let _ = tools;

    tokens
        .to_file_string()
        .map_err(|e| BamlRtError::InvalidArgument(format!("TypeScript render error: {}", e)))
    // Note: genco::Error doesn't implement std::error::Error, so we preserve message context
}
