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
