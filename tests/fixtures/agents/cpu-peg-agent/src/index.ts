/// <reference path="./baml-runtime.d.ts" />
/**
 * Adversarial fixture for issue #341 T1.
 *
 * Burns CPU at module top level so deploying this package via `POST /deploy`
 * exercises the QuickJS-thread starvation path. Concurrent `/readyz` and
 * `/diagnose` probes should still be served, and `runtime_progress_lag_ms`
 * should grow visibly while the loop runs.
 *
 * Target wall-clock duration is ~5s; tuned via `Date.now()` so that runtime
 * speed differences across machines do not change the bound the test asserts
 * against.
 */
import type { SessionResult } from "./baml-runtime";

const CPU_PEG_DURATION_MS = 5_000;
const PEG_STARTED_AT_MS = Date.now();
let cpuPegCounter = 0;
while (Date.now() - PEG_STARTED_AT_MS < CPU_PEG_DURATION_MS) {
  for (let i = 0; i < 100_000; i++) {
    cpuPegCounter = (cpuPegCounter + i) >>> 0;
  }
}

__chat_register({
  run: async (): Promise<SessionResult> => ({
    message: `cpu-peg-agent booted; pegged for ${CPU_PEG_DURATION_MS}ms (counter=${cpuPegCounter}).`,
  }),
});
