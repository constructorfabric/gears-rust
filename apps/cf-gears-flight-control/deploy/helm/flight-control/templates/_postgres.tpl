{{/* Adapt Flight Control's PostgreSQL settings to the common chart's generic inputs. */}}
{{- define "flight-control.postgres-secret" -}}
name: {{ printf "%s-postgres" (include "toolkit-common.fullname" .) | quote }}
stringData:
  pg-password: {{ .Values.postgres.password | quote }}
{{- end -}}

{{- define "flight-control.postgres-config" -}}
{{- $database := deepCopy (.Values.config.data.database | default dict) -}}
{{- $servers := get $database "servers" | default dict -}}
{{- $pg := .Values.postgres -}}
{{- $connection := dict "engine" "postgres" "host" $pg.host "port" $pg.port "user" $pg.user "dbname" $pg.database "password" "${DB_PASSWORD}" -}}
{{- $_ := set $servers "postgres" (mergeOverwrite $connection (get $servers "postgres" | default dict)) -}}
{{- $_ := set $database "servers" $servers -}}
{{- toYaml $database -}}
{{- end -}}
