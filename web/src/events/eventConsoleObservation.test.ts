// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import {
  buildObservationRefreshLoadKey,
  buildObservationScopeKey,
  buildObserveScopeWatchKey,
  shouldPreserveTranscriptOnScopeChange,
  shouldSkipObservationRefresh,
} from "./eventConsoleObservation";

describe("eventConsoleObservation", () => {
  describe("buildObserveScopeWatchKey", () => {
    it("joins context, task, and agent package", () => {
      expect(buildObserveScopeWatchKey("ctx-1", "dispatch-unit-abc", "clickup-agent")).toBe(
        "ctx-1:dispatch-unit-abc:clickup-agent",
      );
    });

    it("uses empty segments when task or agent unset", () => {
      expect(buildObserveScopeWatchKey("ctx-1", null)).toBe("ctx-1::");
    });
  });

  describe("shouldPreserveTranscriptOnScopeChange", () => {
    it("preserves when task resolves on same context during publish", () => {
      expect(
        shouldPreserveTranscriptOnScopeChange(
          "ctx-1:",
          "ctx-1:dispatch-unit-abc",
          true,
        ),
      ).toBe(true);
    });

    it("does not preserve when publish is inactive", () => {
      expect(
        shouldPreserveTranscriptOnScopeChange(
          "ctx-1:",
          "ctx-1:dispatch-unit-abc",
          false,
        ),
      ).toBe(false);
    });

    it("does not preserve when context changes", () => {
      expect(
        shouldPreserveTranscriptOnScopeChange(
          "ctx-1:task-a",
          "ctx-2:task-b",
          true,
        ),
      ).toBe(false);
    });
  });

  describe("buildObservationRefreshLoadKey", () => {
    it("distinguishes preserve vs full reload for coalescing", () => {
      expect(buildObservationRefreshLoadKey("ctx-1", "task-a", true)).toBe(
        "ctx-1:task-a:preserve",
      );
      expect(buildObservationRefreshLoadKey("ctx-1", "task-a", false)).toBe(
        "ctx-1:task-a:full",
      );
    });
  });

  describe("shouldSkipObservationRefresh", () => {
    it("skips redundant full reload when transcript is already loaded", () => {
      expect(
        shouldSkipObservationRefresh("ctx-1:task-a", "ctx-1:task-a", 3, false),
      ).toBe(true);
    });

    it("does not skip when preserve mode is active", () => {
      expect(
        shouldSkipObservationRefresh("ctx-1:task-a", "ctx-1:task-a", 3, true),
      ).toBe(false);
    });

    it("does not skip when transcript is empty", () => {
      expect(
        shouldSkipObservationRefresh("ctx-1:task-a", "ctx-1:task-a", 0, false),
      ).toBe(false);
    });
  });

  describe("buildObservationScopeKey", () => {
    it("trims task id whitespace and includes agent package", () => {
      expect(buildObservationScopeKey("ctx-1", "  task-a  ", "clickup-agent")).toBe(
        "ctx-1:task-a:clickup-agent",
      );
    });
  });
});
