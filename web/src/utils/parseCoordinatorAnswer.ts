export interface ParsedCoordinatorAnswer {
  answer: string;
  confidence: number | null;
  sources: string[];
  actionableGoals: { goal: string; owner?: string; dueDate?: string }[];
  gaps: string[];
  decisions: string[];
  risks: string[];
  followUps: string[];
  clarificationQuestion: string | null;
}

/**
 * Attempts to parse a coordinator agent's rendered answer text.
 * Returns null if the text doesn't match the expected format.
 *
 * Expected format (from renderCoordinatorAnswer in coordinator-agent):
 *   Answer:
 *   <text>
 *
 *   Actionable Goals (...):
 *   - <goal> (owner: X; due: Y)
 *
 *   Sources:
 *   - <url>
 *
 *   Confidence: 0.XX
 *
 *   Gaps:
 *   - <gap>
 *
 *   Clarification:
 *   - <question>
 */
export function parseCoordinatorAnswer(text: string): ParsedCoordinatorAnswer | null {
  if (!text.includes("Answer:") || !text.includes("Confidence:")) return null;

  const sections = splitSections(text);

  const answer = sections["answer"] ?? "";
  const confidenceStr = sections["confidence"]?.trim();
  const confidence = confidenceStr ? parseFloat(confidenceStr) : null;
  if (confidence !== null && isNaN(confidence)) return null;

  const sources = parseBulletList(sections["sources"] ?? "").filter(
    (s) => s !== "None" && s.length > 0,
  );

  // Actionable goals header varies: "Actionable Goals (Owner/Date Present):" or "(Owner/Date Missing In Evidence):"
  const goalsKey = Object.keys(sections).find((k) => k.startsWith("actionable goals"));
  const goalsRaw = parseBulletList(sections[goalsKey ?? ""] ?? "");
  const actionableGoals = goalsRaw
    .filter(
      (g) =>
        !g.startsWith("None identified") &&
        !g.startsWith("Owner/date details"),
    )
    .map(parseGoalLine);

  const gaps = parseBulletList(sections["gaps"] ?? "").filter(
    (g) => g !== "None observed" && g.length > 0,
  );

  const decisions = parseBulletList(sections["decisions"] ?? "").filter(
    (d) => !d.startsWith("None") && d.length > 0,
  );

  const risksKey = Object.keys(sections).find((k) => k.startsWith("risk"));
  const risks = parseBulletList(sections[risksKey ?? ""] ?? "").filter(
    (r) => !r.startsWith("None") && r.length > 0,
  );

  const followUpsKey = Object.keys(sections).find(
    (k) => k.startsWith("follow") || k.startsWith("next steps"),
  );
  const followUps = parseBulletList(sections[followUpsKey ?? ""] ?? "").filter(
    (f) => !f.startsWith("None") && f.length > 0,
  );

  const clarLines = parseBulletList(sections["clarification"] ?? "");
  const clarificationQuestion = clarLines.length > 0 ? clarLines.join(" ") : null;

  return { answer, confidence, sources, actionableGoals, gaps, decisions, risks, followUps, clarificationQuestion };
}

function splitSections(text: string): Record<string, string> {
  const sections: Record<string, string> = {};
  const lines = text.split("\n");
  let currentKey = "";
  let currentLines: string[] = [];

  for (const line of lines) {
    // Match section headers like "Answer:", "Confidence: 0.85", "Actionable Goals (...):"
    // Must not start with "- " (bullet item) or whitespace (continuation)
    const headerMatch = line.match(/^([A-Za-z][A-Za-z\s/()]+):\s*(.*)$/);
    if (headerMatch && !line.startsWith("- ") && !line.startsWith("  ")) {
      if (currentKey) sections[currentKey] = currentLines.join("\n").trim();
      currentKey = headerMatch[1]!.toLowerCase().trim();
      currentLines = headerMatch[2] ? [headerMatch[2]] : [];
    } else {
      currentLines.push(line);
    }
  }
  if (currentKey) sections[currentKey] = currentLines.join("\n").trim();
  return sections;
}

function parseBulletList(text: string): string[] {
  return text
    .split("\n")
    .map((l) => l.replace(/^-\s*/, "").trim())
    .filter((l) => l.length > 0);
}

function parseGoalLine(line: string): { goal: string; owner?: string; dueDate?: string } {
  const parenMatch = line.match(/^(.+?)\s*\((.+)\)$/);
  if (!parenMatch) return { goal: line };
  const goal = parenMatch[1]!.trim();
  const meta = parenMatch[2]!;
  const ownerMatch = meta.match(/owner:\s*([^;]+)/i);
  const dueMatch = meta.match(/due:\s*([^;)]+)/i);
  return {
    goal,
    owner: ownerMatch?.[1]?.trim(),
    dueDate: dueMatch?.[1]?.trim(),
  };
}

/** Safely extract hostname from a URL string; returns raw string on failure. */
export function safeHostname(url: string): string {
  try {
    return new URL(url).hostname;
  } catch {
    return url;
  }
}

/** Check if a string looks like a URL */
export function isUrl(s: string): boolean {
  try {
    const u = new URL(s);
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}
