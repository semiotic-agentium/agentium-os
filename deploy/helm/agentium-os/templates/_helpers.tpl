{{/*
Chart name, or nameOverride if set.
*/}}
{{- define "agentium-os.name" -}}
{{- .Values.nameOverride | default .Chart.Name | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Release-qualified chart name, or fullnameOverride if set.
Truncated to 45 chars so component suffixes (-runner, -surrealdb)
and terminal suffixes (-api, -ingress) stay unique and within
the 63-char Kubernetes name limit.
*/}}
{{- define "agentium-os.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 45 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name (include "agentium-os.name" .) | trunc 45 | trimSuffix "-" }}
{{- end }}
{{- end }}

{{/*
Runner resource name (fullname + "-runner", max 52 chars).
*/}}
{{- define "agentium-os.runner.fullname" -}}
{{- printf "%s-runner" (include "agentium-os.fullname" .) }}
{{- end }}

{{/*
SurrealDB resource name (fullname + "-surrealdb", max 55 chars).
*/}}
{{- define "agentium-os.surrealdb.fullname" -}}
{{- printf "%s-surrealdb" (include "agentium-os.fullname" .) }}
{{- end }}

{{/*
Chart label value (name-version).
*/}}
{{- define "agentium-os.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels applied to every resource.
instance lives in selectorLabels (not here) to stay out of the
immutable StatefulSet selector matchLabels set.
*/}}
{{- define "agentium-os.labels" -}}
helm.sh/chart: {{ include "agentium-os.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- if .Values.global.environment }}
app.kubernetes.io/environment: {{ .Values.global.environment }}
{{- end }}
{{- end }}

{{/*
Runner selector labels — immutable across upgrades.
Used by: runner StatefulSet selector, runner headless Service selector,
runner API Service selector, NetworkPolicy runner pod selectors.
*/}}
{{- define "agentium-os.runner.selectorLabels" -}}
app.kubernetes.io/name: {{ include "agentium-os.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: runner
{{- end }}

{{/*
Full runner labels (common + selector).
*/}}
{{- define "agentium-os.runner.labels" -}}
{{ include "agentium-os.labels" . }}
{{ include "agentium-os.runner.selectorLabels" . }}
{{- end }}

{{/*
SurrealDB selector labels — immutable across upgrades.
Used by: SurrealDB StatefulSet selector, SurrealDB Service selector,
NetworkPolicy surrealdb pod selectors.
*/}}
{{- define "agentium-os.surrealdb.selectorLabels" -}}
app.kubernetes.io/name: {{ include "agentium-os.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: surrealdb
{{- end }}

{{/*
Full SurrealDB labels (common + selector).
*/}}
{{- define "agentium-os.surrealdb.labels" -}}
{{ include "agentium-os.labels" . }}
{{ include "agentium-os.surrealdb.selectorLabels" . }}
{{- end }}
