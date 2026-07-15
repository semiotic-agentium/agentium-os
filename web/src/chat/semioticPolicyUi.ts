// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import type { SemioticPosture } from "../types/config";

export function postureLabel(posture: SemioticPosture): string {
  switch (posture) {
    case "off":
      return "Off";
    case "audit":
      return "Audit";
    case "enforce":
      return "Enforce";
    default:
      return posture;
  }
}

export function postureChipClass(posture: SemioticPosture): string {
  switch (posture) {
    case "off":
      return "semiotic-posture-chip semiotic-posture-chip--off";
    case "audit":
      return "semiotic-posture-chip semiotic-posture-chip--audit";
    case "enforce":
      return "semiotic-posture-chip semiotic-posture-chip--enforce";
    default:
      return "semiotic-posture-chip";
  }
}

export function incidentSeverityClass(severity: string): string {
  switch (severity) {
    case "critical":
      return "semiotic-incident--critical";
    case "warning":
      return "semiotic-incident--warning";
    default:
      return "semiotic-incident--info";
  }
}

export function formatIncidentTime(ms: number): string {
  if (!ms) return "—";
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return String(ms);
  }
}

export { preventionRatioLabel } from "../utils/gateHelpers";
