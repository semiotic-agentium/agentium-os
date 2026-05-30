// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/** Provenance used to store these literals for non-text parts; strip when hydrating old rows. */
const LEGACY_LINE_PLACEHOLDERS = new Set([
  "[structured-data part]",
  "[raw part]",
  "[file part]",
]);

/** Remove legacy placeholder lines from assistant/user message text projected from the graph. */
export function stripLegacyStructuredPlaceholderLines(text: string): string {
  return text
    .split(/\r?\n/)
    .filter((line) => !LEGACY_LINE_PLACEHOLDERS.has(line.trim()))
    .join("\n")
    .trim();
}
