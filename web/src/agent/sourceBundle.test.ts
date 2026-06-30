// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { buildPublishCommandFromFiles } from "./sourceBundle";

const FIXTURE_ROOT = join(
  import.meta.dirname,
  "../../../tests/fixtures/agents/task-lifecycle-demo",
);
const AGENT_NAME = "task-lifecycle-demo";

function collectFixtureFiles(dir: string): File[] {
  const out: File[] = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) {
      out.push(...collectFixtureFiles(full));
      continue;
    }
    const rel = relative(FIXTURE_ROOT, full).replace(/\\/g, "/");
    const webkitRelativePath = `${AGENT_NAME}/${rel}`;
    out.push({
      name: rel,
      webkitRelativePath,
      text: () => Promise.resolve(readFileSync(full, "utf8")),
    } as File);
  }
  return out;
}

describe("buildPublishCommandFromFiles", () => {
  it("builds a source bundle from task-lifecycle-demo layout", async () => {
    const files = collectFixtureFiles(FIXTURE_ROOT);
    const cmd = await buildPublishCommandFromFiles(files, { rationale: "test" });

    expect(cmd.name).toBe("task-lifecycle-demo");
    expect(cmd.rationale).toBe("test");
    expect(cmd.origin).toBe("Original");
    expect(cmd.source.ts_sources.length).toBeGreaterThan(0);
    expect(cmd.source.baml_sources.length).toBeGreaterThan(0);
    expect(cmd.source.manifest.name).toBe("task-lifecycle-demo");
  });

  it("rejects empty file list", async () => {
    await expect(buildPublishCommandFromFiles([])).rejects.toThrow(/No files/);
  });
});
