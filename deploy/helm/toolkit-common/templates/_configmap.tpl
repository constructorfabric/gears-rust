{{/*
toolkit-common: ConfigMap holding the gear's YAML config.

`.Values.config.data` (a structured values tree) is rendered with `toYaml` then
`tpl`: every leaf is overridable via Helm value merges, while embedded template
expressions in string values (e.g. `{{ .Values.postgres.host }}`) still resolve.
Mounted at `.Values.config.mountPath`. Rendered only when `config.data` is set.
*/}}
{{- define "toolkit-common.configmap" -}}
{{- if .Values.config.data -}}
apiVersion: v1
kind: ConfigMap
metadata:
  name: {{ include "toolkit-common.fullname" . }}
  labels:
    {{- include "toolkit-common.labels" . | nindent 4 }}
data:
  {{ .Values.config.fileName }}: |
    {{- tpl (toYaml .Values.config.data) . | nindent 4 }}
{{- end -}}
{{- end -}}
