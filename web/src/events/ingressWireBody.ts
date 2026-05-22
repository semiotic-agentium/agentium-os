/** Must match `baml_rt_core::host_source_records_body::INGRESS_WIRE_BODY_DELIMITER`. */
export const INGRESS_WIRE_BODY_DELIMITER = "--- host.source-records.v1 ---";

export function isIngressWireBody(text: string): boolean {
  return text.trimStart().startsWith(INGRESS_WIRE_BODY_DELIMITER);
}

/** Pretty-print wire JSON from an ingress user row for operator display. */
export function ingressWireBodyForDisplay(text: string): string {
  const trimmed = text.trimStart();
  if (!trimmed.startsWith(INGRESS_WIRE_BODY_DELIMITER)) {
    return text;
  }
  const jsonPart = trimmed.slice(INGRESS_WIRE_BODY_DELIMITER.length).trimStart();
  try {
    return JSON.stringify(JSON.parse(jsonPart), null, 2);
  } catch {
    return jsonPart;
  }
}
