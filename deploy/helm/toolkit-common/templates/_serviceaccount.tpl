{{/* ServiceAccount for projected-token authentication. */}}
{{- define "toolkit-common.serviceAccount" -}}
{{- if .Values.serviceAccount.create -}}
apiVersion: v1
kind: ServiceAccount
metadata:
  name: {{ include "toolkit-common.serviceAccountName" . }}
  labels:
    {{- include "toolkit-common.labels" . | nindent 4 }}
  {{- with .Values.serviceAccount.annotations }}
  annotations:
    {{- toYaml . | nindent 4 }}
  {{- end }}
automountServiceAccountToken: {{ if hasKey .Values.serviceAccount "automount" }}{{ .Values.serviceAccount.automount }}{{ else }}true{{ end }}
{{- end -}}
{{- end -}}
