/**
 * Host / client placeholder copy for TASK_STATE_INPUT_REQUIRED when the agent did not
 * supply a real `awaitInput(prompt)`. Must not appear as transcript text — only UI chrome.
 */
export function isSyntheticInputRequiredPrompt(raw: string): boolean {
  const t = raw.trim();
  if (!t) return true;
  const n = t
    .toLowerCase()
    .replace(/\s+/g, " ")
    .replace(/[.…]+$/u, "")
    .trim();
  return (
    n === "reply to continue" ||
    n === "reply to continue the conversation" ||
    /^reply to continue(\s+the conversation)?$/iu.test(n)
  );
}
