// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

// Thin HTTP client for A2A load testing. Node 22 built-ins only (fetch +
// AbortController + performance.now). No external deps.

// Exhaustive set of error categories this module can emit. Aggregators
// initialise per-category counters from this list to stay in lockstep if
// a category is added.
export const ERROR_CATEGORIES = Object.freeze([
  "http_error",
  "payload_mismatch",
  "network_error",
  "timeout",
]);

export async function sendAndTime({ url, body, timeoutMs, expectSubstring }) {
  const controller = new AbortController();
  // Abort with no reason so fetch() throws a standard AbortError, which our
  // catch block can classify as "timeout". Passing a reason overrides the
  // error name and the timeout gets misclassified as network_error.
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  const t0 = performance.now();
  let httpStatus = -1;
  let bodyText = "";
  let bodyBytes = 0;
  let timeToHeadersMs = null;
  let timeToCompleteMs = null;
  let error = null;
  let errorCategory = null;

  try {
    const response = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    timeToHeadersMs = performance.now() - t0;
    httpStatus = response.status;
    bodyText = await response.text();
    timeToCompleteMs = performance.now() - t0;
    bodyBytes = Buffer.byteLength(bodyText, "utf8");

    if (httpStatus !== 200) {
      errorCategory = "http_error";
      error = `HTTP ${httpStatus}: ${bodyText.slice(0, 200)}`;
    } else if (expectSubstring && !bodyText.includes(expectSubstring)) {
      errorCategory = "payload_mismatch";
      error = `response missing expected substring: ${bodyText.slice(0, 200)}`;
    }
  } catch (err) {
    timeToCompleteMs = performance.now() - t0;
    // AbortController cancellations come through as DOMException
    // { name: "AbortError" } OR as plain errors — checking signal.aborted
    // is the reliable signal that our setTimeout fired.
    if (controller.signal.aborted || (err && err.name === "AbortError")) {
      errorCategory = "timeout";
      error = `timeout after ${timeoutMs}ms`;
    } else {
      errorCategory = "network_error";
      error = err instanceof Error ? err.message : String(err);
    }
  } finally {
    clearTimeout(timer);
  }

  return {
    httpStatus,
    timeToHeadersMs,
    timeToCompleteMs,
    bodyBytes,
    errorCategory,
    error,
  };
}
