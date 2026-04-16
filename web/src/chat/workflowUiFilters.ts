/** Workflow phase lines rendered in `WorkflowProgress`; hide from chat bubbles. */
export function isWorkflowStatusText(text: string): boolean {
  const t = text.trim();
  return (
    /^Discovering available specialist agents/i.test(t) ||
    /^Planning workflow/i.test(t) ||
    /^Executing\s+\d+\s+workflow\s+node/i.test(t) ||
    /^Workflow execution pass\s+\d+/i.test(t) ||
    /^Synthesiz/i.test(t) ||
    /^Compiling final/i.test(t)
  );
}
