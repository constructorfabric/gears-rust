{{/* Optional HorizontalPodAutoscaler. Enabled per-chart via autoscaling.enabled. */}}
{{- define "toolkit-common.hpa" -}}
{{- $hpa := .Values.autoscaling | default dict -}}
{{- if $hpa.enabled -}}
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: {{ include "toolkit-common.fullname" . }}
  labels:
    {{- include "toolkit-common.labels" . | nindent 4 }}
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: {{ include "toolkit-common.fullname" . }}
  minReplicas: {{ $hpa.minReplicas | default 1 }}
  maxReplicas: {{ $hpa.maxReplicas | default 3 }}
  metrics:
    {{- if $hpa.targetCPUUtilizationPercentage }}
    - type: Resource
      resource:
        name: cpu
        target:
          type: Utilization
          averageUtilization: {{ $hpa.targetCPUUtilizationPercentage }}
    {{- end }}
    {{- if $hpa.targetMemoryUtilizationPercentage }}
    - type: Resource
      resource:
        name: memory
        target:
          type: Utilization
          averageUtilization: {{ $hpa.targetMemoryUtilizationPercentage }}
    {{- end }}
  {{- with $hpa.behavior }}
  behavior:
    {{- toYaml . | nindent 4 }}
  {{- end }}
{{- end -}}
{{- end -}}
