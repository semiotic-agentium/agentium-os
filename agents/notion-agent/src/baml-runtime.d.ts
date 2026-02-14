/**
 * BAML runtime TypeScript declarations.
 * Auto-generated from BAML runtime IR — do not edit manually.
 * Use these types and function declarations in your agent code (e.g. index.ts).
 */

/** BAML functions: call these from your agent (e.g. await MyFunction(args)). Declared in global scope so they are visible when this file is used as a module. */

declare global {

declare function ChooseNotionAction(args?: Record<string, unknown>): Promise<unknown>;

}

/** Runtime interaction API: A2A task FSM (message-first, typestate rails). */

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
        ? "TASK_STATE_WORKING" | "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED" | "TASK_STATE_REJECTED"
    : S extends "TASK_STATE_AUTH_REQUIRED"
        ? "TASK_STATE_WORKING" | "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED" | "TASK_STATE_REJECTED"
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
export type A2aMessageEvent<S extends A2aTaskState = A2aTaskState> =
    | { kind: "assistantMessage"; text: string; task: A2aTaskView<S> }
    | { kind: "statusChanged"; from?: A2aTaskState; to: S; task: A2aTaskView<S> }
    | { kind: "artifactPublished"; task: A2aTaskView<S>; artifact: unknown; append?: boolean; lastChunk?: boolean }
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
declare function openA2aTaskSession<I = unknown>(token: string): Promise<A2aSessionAwaitingInput<I>>;

/**
 * Host-native ReAct loop helper.
 * Executes the plan function and tool calls in Rust; JS only passes options.
 */
export interface ReActLoopHostOptions {
    planFunction: string;
    userMessage: string;
    maxSteps?: number;
    dedupe?: boolean;
}
declare function runReActLoopHost(token: string, opts: ReActLoopHostOptions): Promise<string>;
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
