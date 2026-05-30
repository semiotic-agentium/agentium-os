// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

// Concurrency driver: N workers cycle through warmup (records dropped) then
// measured (records kept). Each worker alternates through the full target
// list per-request (round-robin) so traffic is distributed across all
// ingress URLs regardless of concurrency — at c=1 with two targets the
// single worker alternates 50/50 between them.

import { sendAndTime } from "./http-client.mjs";
import { buildSendStreamBody, nextIds } from "./payload.mjs";

const DEFAULT_REQUEST_TIMEOUT_MS = 30_000;

export async function runConcurrencyLevel({
  concurrency,
  warmupSeconds,
  measuredSeconds,
  targets,
  payloadText,
  expectSubstring,
  requestTimeoutMs = DEFAULT_REQUEST_TIMEOUT_MS,
  onPhaseChange = () => {},
}) {
  if (!targets.length) {
    throw new Error("targets must be non-empty");
  }

  const warmupEnd = performance.now() + warmupSeconds * 1000;
  const measuredStart = warmupEnd;
  const measuredEnd = measuredStart + measuredSeconds * 1000;

  const measuredRecords = [];
  let phaseNotified = "warmup";
  onPhaseChange("warmup");

  const worker = async (workerIndex) => {
    let reqIndex = 0;
    while (performance.now() < measuredEnd) {
      const targetUrl = targets[(workerIndex + reqIndex) % targets.length];
      reqIndex += 1;
      const { messageId, correlationId } = nextIds();
      const body = buildSendStreamBody({
        text: payloadText,
        messageId,
        correlationId,
      });
      const result = await sendAndTime({
        url: targetUrl,
        body,
        timeoutMs: requestTimeoutMs,
        expectSubstring,
      });
      const now = performance.now();
      if (now >= measuredStart && now < measuredEnd) {
        measuredRecords.push({
          targetUrl,
          ...result,
        });
      }
    }
  };

  const phaseTicker = setInterval(() => {
    if (phaseNotified === "warmup" && performance.now() >= warmupEnd) {
      phaseNotified = "measured";
      onPhaseChange("measured");
    }
  }, 200);

  try {
    await Promise.all(
      Array.from({ length: concurrency }, (_, i) => worker(i)),
    );
  } finally {
    clearInterval(phaseTicker);
  }

  onPhaseChange("done");

  return {
    concurrency,
    warmupSeconds,
    measuredSeconds,
    records: measuredRecords,
  };
}
