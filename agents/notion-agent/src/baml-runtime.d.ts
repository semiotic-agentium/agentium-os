declare function ChooseNotionAction(args?: Record<string, unknown>): Promise<unknown>;
declare function SummarizeNotionContent(args?: Record<string, unknown>): Promise<unknown>;

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
export type ToolName = "support/notion";
type NotionAction = "SearchPages" | "GetPage" | "GetPageBlocks";
type NotionInput = { action: NotionAction, query: string | null, page_id: string | null, block_id: string | null, start_cursor: string | null, page_size: number | null, };
type NotionOutput = { pages: Array<NotionPageSummary>, blocks: Array<NotionBlockSummary>, next_cursor: string | null, has_more: boolean, sources: Array<NotionSource>, message: string, };
type NotionPageSummary = { id: string, title: string, url: string, last_edited_time: string | null, };
type NotionBlockSummary = { id: string, block_type: string, text: string | null, has_children: boolean, };
type NotionSource = { page_id: string, url: string, };
type NotionSummary = { summary: string, };
export interface ToolInputMap { "support/notion": NotionInput;
 }export interface ToolOutputMap { "support/notion": NotionOutput;
 }export type ToolInput<T extends ToolName> = ToolInputMap[T];export type ToolOutput<T extends ToolName> = ToolOutputMap[T];declare function openToolSession<T extends ToolName>(toolName: T): Promise<ToolSession<ToolInput<T>, ToolOutput<T>>>;
declare function openSupportNotionSession(): Promise<ToolSession<ToolInput<"support/notion">, ToolOutput<"support/notion">>>;
