import { describe, expect, it } from "vitest";
import { encodeContextIdForPath } from "../contextPath";

describe("encodeContextIdForPath", () => {
  it("encodes colons and slashes in delegated a2a context ids", () => {
    const raw =
      "a2a:ctx-1779923250040-f3f44ac63fd8b156:grafana-investigator/default:a2a-child-51e1cf94";
    const encoded = encodeContextIdForPath(raw);
    expect(encoded).not.toContain("/");
    expect(encoded).toContain("%2F");
    expect(encoded).toContain("%3A");
    const path = `/contexts/${encoded}/conversation-history/stream`;
    expect(path.split("/")).toEqual([
      "",
      "contexts",
      encoded,
      "conversation-history",
      "stream",
    ]);
  });

  it("leaves simple ctx ids mostly unchanged aside from safe chars", () => {
    expect(encodeContextIdForPath("ctx-1779923250040-abc")).toBe(
      "ctx-1779923250040-abc",
    );
  });
});
