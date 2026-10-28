{{/* PostgreSQL credentials and optional values.secrets[] (opt-in via a secrets.yaml wrapper). */}}
{{- define "toolkit-common.secrets" -}}
{{- $root := . -}}
{{- with .Values.postgres }}
---
apiVersion: v1
kind: Secret
metadata:
  name: {{ include "toolkit-common.fullname" $root }}-postgres
  labels:
    {{- include "toolkit-common.labels" $root | nindent 4 }}
type: Opaque
stringData:
  pg-password: {{ .password | quote }}
{{- end }}
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
