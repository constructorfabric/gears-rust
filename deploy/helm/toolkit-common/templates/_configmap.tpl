{{/* config.data is rendered with Helm `tpl`; escape literal {{ or }} in config content. */}}
{{- define "toolkit-common.configmap" -}}
{{- if .Values.config.data -}}
{{- $config := deepCopy .Values.config.data -}}
{{- with .Values.postgres -}}
{{- $database := get $config "database" | default dict -}}
{{- $servers := get $database "servers" | default dict -}}
{{- $connection := dict "engine" "postgres" "host" .host "port" .port "user" .user "dbname" .database "password" "${DB_PASSWORD}" -}}
{{- $_ := set $servers "postgres" (mergeOverwrite $connection (get $servers "postgres" | default dict)) -}}
{{- $_ := set $database "servers" $servers -}}
{{- $_ := set $config "database" $database -}}
{{- end -}}
apiVersion: v1
kind: ConfigMap
metadata:
  name: {{ include "toolkit-common.fullname" . }}
  labels:
    {{- include "toolkit-common.labels" . | nindent 4 }}
data:
  {{ .Values.config.fileName }}: |
    {{- tpl (toYaml $config) . | nindent 4 }}
{{- end -}}
{{- end -}}
