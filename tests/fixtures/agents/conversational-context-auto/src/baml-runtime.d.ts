declare function ChatWithContext(args?: Record<string, unknown>): Promise<unknown>;

declare function ChooseCalcTool(args?: Record<string, unknown>): Promise<unknown>;

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
export type ToolName = "support/calculate";
type CalculatorInput = { expression: Expression, };
type CalculatorOutput = { expression: string, result: number, formatted: string, };
export interface ToolInputMap { "support/calculate": CalculatorInput;
 }export interface ToolOutputMap { "support/calculate": CalculatorOutput;
 }export type ToolInput<T extends ToolName> = ToolInputMap[T];export type ToolOutput<T extends ToolName> = ToolOutputMap[T];declare function openToolSession<T extends ToolName>(toolName: T): Promise<ToolSession<ToolInput<T>, ToolOutput<T>>>;
declare function openSupportCalculateSession(): Promise<ToolSession<ToolInput<"support/calculate">, ToolOutput<"support/calculate">>>;
