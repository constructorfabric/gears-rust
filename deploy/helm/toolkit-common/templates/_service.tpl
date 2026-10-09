{{/* HTTP Service with optional gRPC port. */}}
{{- define "toolkit-common.service" -}}
apiVersion: v1
kind: Service
metadata:
  name: {{ include "toolkit-common.fullname" . }}
  labels:
    {{- include "toolkit-common.labels" . | nindent 4 }}
spec:
  type: {{ .Values.service.type }}
  ports:
    - port: {{ .Values.service.port }}
      targetPort: http
      protocol: TCP
      name: http
      {{- if and (eq .Values.service.type "NodePort") .Values.service.nodePort }}
      nodePort: {{ .Values.service.nodePort }}
      {{- end }}
    {{- if .Values.grpc.enabled }}
    - port: {{ .Values.grpc.port }}
      targetPort: grpc
      protocol: TCP
      name: grpc
    {{- end }}
  selector:
    {{- include "toolkit-common.selectorLabels" . | nindent 4 }}
{{- end -}}
