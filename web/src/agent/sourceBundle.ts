// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/** Build repository `PublishCommand.source` from browser folder upload (mirrors Rust `source_bundle_from_agent_dir`). */

export interface SourceFile {
  path: string;
  content: string;
}

export interface SourceBundle {
  manifest: Record<string, unknown>;
  ts_sources: SourceFile[];
  baml_sources: SourceFile[];
}

export interface PublishCommandPayload {
  name: string;
  source: SourceBundle;
  rationale: string;
  origin: "Original" | "Iteration";
}

export class SourceBundleError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SourceBundleError";
  }
}

function normalizeRelativePath(raw: string): string {
  const p = raw.replace(/\\/g, "/").replace(/^\/+/, "");
  if (!p || p.includes("..") || p.includes("\0")) {
    throw new SourceBundleError(`Invalid source path: ${raw}`);
  }
  return p;
}

function fileRelativePath(file: File, rootPrefix: string): string {
  const raw = (file.webkitRelativePath || file.name).replace(/\\/g, "/");
  const normalized = normalizeRelativePath(raw);
  if (rootPrefix && !normalized.startsWith(`${rootPrefix}/`) && normalized !== rootPrefix) {
    throw new SourceBundleError(`File outside agent root: ${normalized}`);
  }
  return normalized;
}

async function readFileText(file: File): Promise<string> {
  return file.text();
}

/**
 * Build a publish payload from a directory picker (`webkitdirectory`) FileList.
 * Expects paths like `my-agent/manifest.json`, `my-agent/src/index.ts`, …
 */
export async function buildPublishCommandFromFiles(
  files: FileList | File[],
  options?: { rationale?: string; origin?: "Original" | "Iteration" },
): Promise<PublishCommandPayload> {
  const list = Array.from(files);
  if (list.length === 0) {
    throw new SourceBundleError("No files selected.");
  }

  const firstPath = (list[0]?.webkitRelativePath || list[0]?.name || "").replace(/\\/g, "/");
  const rootDir = firstPath.includes("/") ? (firstPath.split("/")[0] ?? "") : "";
  if (!rootDir) {
    throw new SourceBundleError(
      "Select an agent folder (directory upload), not individual files.",
    );
  }

  let manifestFile: File | null = null;
  const tsSources: SourceFile[] = [];
  const bamlSources: SourceFile[] = [];

  for (const file of list) {
    const rel = fileRelativePath(file, rootDir);
    if (rel === `${rootDir}/manifest.json`) {
      manifestFile = file;
      continue;
    }
    if (rel.startsWith(`${rootDir}/src/`) && rel.endsWith(".ts")) {
      tsSources.push({ path: rel.slice(rootDir.length + 1), content: await readFileText(file) });
      continue;
    }
    if (rel.startsWith(`${rootDir}/baml_src/`) && rel.endsWith(".baml")) {
      bamlSources.push({ path: rel.slice(rootDir.length + 1), content: await readFileText(file) });
    }
  }

  if (!manifestFile) {
    throw new SourceBundleError(`Missing ${rootDir}/manifest.json in selected folder.`);
  }

  let manifest: Record<string, unknown>;
  try {
    manifest = JSON.parse(await readFileText(manifestFile)) as Record<string, unknown>;
  } catch {
    throw new SourceBundleError("manifest.json is not valid JSON.");
  }

  const name = manifest.name;
  if (typeof name !== "string" || name.trim().length === 0) {
    throw new SourceBundleError("manifest.json must include a non-empty \"name\" field.");
  }

  tsSources.sort((a, b) => a.path.localeCompare(b.path));
  bamlSources.sort((a, b) => a.path.localeCompare(b.path));

  return {
    name: name.trim(),
    source: {
      manifest,
      ts_sources: tsSources,
      baml_sources: bamlSources,
    },
    rationale: options?.rationale?.trim() || "Loaded from dev console",
    origin: options?.origin ?? "Original",
  };
}

/** Summarize selected files before upload. */
export function summarizeAgentFiles(files: FileList | File[]): {
  rootDir: string;
  tsCount: number;
  bamlCount: number;
  hasManifest: boolean;
} {
  const list = Array.from(files);
  const firstPath = (list[0]?.webkitRelativePath || list[0]?.name || "").replace(/\\/g, "/");
  const rootDir = firstPath.includes("/") ? (firstPath.split("/")[0] ?? "") : "";
  let tsCount = 0;
  let bamlCount = 0;
  let hasManifest = false;
  for (const file of list) {
    const rel = (file.webkitRelativePath || file.name).replace(/\\/g, "/");
    if (rel.endsWith("/manifest.json") || rel === "manifest.json") hasManifest = true;
    if (rel.includes("/src/") && rel.endsWith(".ts")) tsCount += 1;
    if (rel.includes("/baml_src/") && rel.endsWith(".baml")) bamlCount += 1;
  }
  return { rootDir, tsCount, bamlCount, hasManifest };
}
