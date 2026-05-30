// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

// Zero-dependency percentile + histogram helpers for load-test summaries.
//
// Inputs are assumed to be finite non-negative millisecond values. Callers
// should filter NaN / null before passing.

export function percentile(sorted, p) {
  if (!sorted.length) return null;
  if (p <= 0) return sorted[0];
  if (p >= 100) return sorted[sorted.length - 1];
  const idx = (p / 100) * (sorted.length - 1);
  const lo = Math.floor(idx);
  const hi = Math.ceil(idx);
  if (lo === hi) return sorted[lo];
  const frac = idx - lo;
  return sorted[lo] * (1 - frac) + sorted[hi] * frac;
}

export function summarize(values) {
  if (!values.length) {
    return { count: 0, min: null, max: null, mean: null, p50: null, p90: null, p95: null, p99: null };
  }
  const sorted = [...values].sort((a, b) => a - b);
  const sum = sorted.reduce((acc, v) => acc + v, 0);
  return {
    count: sorted.length,
    min: sorted[0],
    max: sorted[sorted.length - 1],
    mean: sum / sorted.length,
    p50: percentile(sorted, 50),
    p90: percentile(sorted, 90),
    p95: percentile(sorted, 95),
    p99: percentile(sorted, 99),
  };
}

export function round(value, digits = 2) {
  if (value === null || value === undefined || Number.isNaN(value)) return null;
  const factor = 10 ** digits;
  return Math.round(value * factor) / factor;
}

export function roundSummary(summary, digits = 2) {
  if (!summary) return summary;
  return {
    count: summary.count,
    min: round(summary.min, digits),
    max: round(summary.max, digits),
    mean: round(summary.mean, digits),
    p50: round(summary.p50, digits),
    p90: round(summary.p90, digits),
    p95: round(summary.p95, digits),
    p99: round(summary.p99, digits),
  };
}
