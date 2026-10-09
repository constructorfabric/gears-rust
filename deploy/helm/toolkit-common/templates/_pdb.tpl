{{/* Optional PodDisruptionBudget. Enabled per-chart via pdb.enabled. */}}
{{- define "toolkit-common.pdb" -}}
{{- $pdb := .Values.pdb | default dict -}}
{{- if $pdb.enabled -}}
apiVersion: policy/v1
kind: PodDisruptionBudget
metadata:
  name: {{ include "toolkit-common.fullname" . }}
  labels:
    {{- include "toolkit-common.labels" . | nindent 4 }}
spec:
  {{- if hasKey $pdb "minAvailable" }}
  minAvailable: {{ $pdb.minAvailable }}
  {{- else if hasKey $pdb "maxUnavailable" }}
  maxUnavailable: {{ $pdb.maxUnavailable }}
  {{- else }}
  minAvailable: 1
  {{- end }}
  selector:
    matchLabels:
      {{- include "toolkit-common.selectorLabels" . | nindent 6 }}
{{- end -}}
{{- end -}}
