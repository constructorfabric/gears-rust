{{/* Optional NetworkPolicy (opt-in). Rules are operator-supplied; no default-deny. */}}
{{- define "toolkit-common.networkPolicy" -}}
{{- $np := .Values.networkPolicy | default dict -}}
{{- if $np.enabled -}}
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: {{ include "toolkit-common.fullname" . }}
  labels:
    {{- include "toolkit-common.labels" . | nindent 4 }}
spec:
  podSelector:
    matchLabels:
      {{- include "toolkit-common.selectorLabels" . | nindent 6 }}
  policyTypes:
    {{- toYaml ($np.policyTypes | default (list "Ingress" "Egress")) | nindent 4 }}
  {{- with $np.ingress }}
  ingress:
    {{- toYaml . | nindent 4 }}
  {{- end }}
  {{- with $np.egress }}
  egress:
    {{- toYaml . | nindent 4 }}
  {{- end }}
{{- end -}}
{{- end -}}
