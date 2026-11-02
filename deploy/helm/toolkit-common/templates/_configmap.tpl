{{/* config.data is rendered with Helm `tpl`; escape literal {{ or }} in config content. */}}
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
