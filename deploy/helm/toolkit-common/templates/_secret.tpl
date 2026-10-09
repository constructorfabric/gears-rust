{{/* Optional Secrets rendered from values.secrets[] (opt-in via a secrets.yaml wrapper). */}}
{{- define "toolkit-common.secrets" -}}
{{- $root := . -}}
{{- range .Values.secrets | default list }}
{{- if .stringData }}
---
apiVersion: v1
kind: Secret
metadata:
  name: {{ required "secrets[].name is required when using secrets[]" .name }}
  labels:
    {{- include "toolkit-common.labels" $root | nindent 4 }}
  {{- with .annotations }}
  annotations:
    {{- toYaml . | nindent 4 }}
  {{- end }}
type: {{ default "Opaque" .type }}
stringData:
  {{- tpl (toYaml .stringData) $root | nindent 2 }}
{{- end -}}
{{- end }}
{{- end -}}
