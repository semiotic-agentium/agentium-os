/**
 * BAML runtime TypeScript declarations.
 * Auto-generated from BAML runtime IR — do not edit manually.
 * Use these types and function declarations in your agent code (e.g. index.ts).
 */

/** Types for BAML function arguments and return values (classes, enums, aliases). */

/** BAML functions: call these from your agent (e.g. await MyFunction(args)). Declared in global scope so they are visible when this file is used as a module. */

declare global {

declare function PersonaChat(args: { user_message: string }): Promise<string>;

}

/** Host tool session API: openToolSession, tool-specific openers, and shared types (ToolFailure, ToolStep, etc.). */

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
export type ToolStep<O> =
    | { status: "streaming"; output: O }
    | { status: "done"; output?: O }
    | { status: "error"; error: ToolFailure };
export interface ToolSession<I, O> {
    sessionId: string;
    send(input: I): Promise<void>;
    continue(): Promise<ToolStep<O>>;
    finish(): Promise<void>;
    abort(reason?: string): Promise<void>;
}
export type ToolName = never;
