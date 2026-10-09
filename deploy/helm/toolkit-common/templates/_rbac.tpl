{{/* TokenReview permission for this chart's ServiceAccount. */}}
{{- define "toolkit-common.rbac" -}}
{{- if and .Values.saToken.enabled .Values.serviceAccount.create }}
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: {{ include "toolkit-common.fullname" . }}-auth-delegator
  labels:
    {{- include "toolkit-common.labels" . | nindent 4 }}
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: system:auth-delegator
subjects:
  - kind: ServiceAccount
    name: {{ include "toolkit-common.serviceAccountName" . }}
    namespace: {{ .Release.Namespace }}
{{- end }}
{{- end -}}
