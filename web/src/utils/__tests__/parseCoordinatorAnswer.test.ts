import { describe, it, expect } from "vitest";
import {
  parseCoordinatorAnswer,
  safeHostname,
  isUrl,
} from "../parseCoordinatorAnswer";

const SAMPLE_TEXT = `Answer:
The project uses a monorepo structure with Cargo workspaces.

Actionable Goals (Owner/Date Present):
- Refactor auth module (owner: Alice; due: 2025-06-01)
- Add integration tests (owner: Bob)

Sources:
- https://example.com/docs
- https://github.com/org/repo

Confidence: 0.85

Gaps:
- Missing deployment documentation
- None observed

Decisions:
- Use Axum over Actix for HTTP

Risks:
- Dependency on nightly Rust

Follow Ups:
- Review CI pipeline
- Add coverage reporting

Clarification:
- Should we target stable Rust?`;

describe("parseCoordinatorAnswer", () => {
  it("parses a well-formed coordinator answer", () => {
    const result = parseCoordinatorAnswer(SAMPLE_TEXT);
    expect(result).not.toBeNull();
    expect(result!.answer).toBe("The project uses a monorepo structure with Cargo workspaces.");
    expect(result!.confidence).toBe(0.85);
    expect(result!.sources).toEqual([
      "https://example.com/docs",
      "https://github.com/org/repo",
    ]);
    expect(result!.actionableGoals).toHaveLength(2);
    expect(result!.actionableGoals[0]).toEqual({
      goal: "Refactor auth module",
      owner: "Alice",
      dueDate: "2025-06-01",
    });
    expect(result!.actionableGoals[1]).toEqual({
      goal: "Add integration tests",
      owner: "Bob",
      dueDate: undefined,
    });
    expect(result!.gaps).toEqual(["Missing deployment documentation"]);
    expect(result!.decisions).toEqual(["Use Axum over Actix for HTTP"]);
    expect(result!.risks).toEqual(["Dependency on nightly Rust"]);
    expect(result!.followUps).toEqual(["Review CI pipeline", "Add coverage reporting"]);
    expect(result!.clarificationQuestion).toBe("Should we target stable Rust?");
  });

  it("returns null for text without Answer: header", () => {
    expect(parseCoordinatorAnswer("Some random text\nConfidence: 0.5")).toBeNull();
  });

  it("returns null for text without Confidence: header", () => {
    expect(parseCoordinatorAnswer("Answer:\nSome text")).toBeNull();
  });

  it("returns null for invalid confidence value", () => {
    expect(parseCoordinatorAnswer("Answer:\nText\n\nConfidence: not-a-number")).toBeNull();
  });

  it("filters out 'None' sources", () => {
    const text = "Answer:\nText\n\nSources:\n- None\n\nConfidence: 0.5";
    const result = parseCoordinatorAnswer(text);
    expect(result!.sources).toEqual([]);
  });
});

describe("safeHostname", () => {
  it("extracts hostname from valid URL", () => {
    expect(safeHostname("https://example.com/path")).toBe("example.com");
  });
  it("returns raw string for invalid URL", () => {
    expect(safeHostname("not-a-url")).toBe("not-a-url");
  });
});

describe("isUrl", () => {
  it("returns true for http URL", () => {
    expect(isUrl("http://example.com")).toBe(true);
  });
  it("returns true for https URL", () => {
    expect(isUrl("https://example.com/path?q=1")).toBe(true);
  });
  it("returns false for non-URL", () => {
    expect(isUrl("just a string")).toBe(false);
  });
  it("returns false for ftp URL", () => {
    expect(isUrl("ftp://example.com")).toBe(false);
  });
});
