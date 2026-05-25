export {};

declare global {
  type ToolSessionHandle = {
    send(args: Record<string, unknown>): Promise<unknown>;
    continue(readInput?: Record<string, unknown>): Promise<unknown>;
    finish(): Promise<unknown>;
    abort(reason?: string): Promise<unknown>;
  };

  function openToolSession(
    toolName: string,
    openInput?: Record<string, unknown>,
  ): Promise<ToolSessionHandle>;
}
