{{/* Shared Deployment for all gears. */}}
{{- define "toolkit-common.deployment" -}}
{{- $root := . -}}
{{- $global := .Values.global | default dict -}}
{{- $otel := get $global "opentelemetry" | default dict -}}
{{- $registry := $global.imageRegistry | default .Values.image.registry | default "" -}}
{{- $renderedSecrets := include "toolkit-common.secrets" . | trim -}}
apiVersion: apps/v1
kind: Deployment
metadata:
  name: {{ include "toolkit-common.fullname" . }}
  labels:
    {{- include "toolkit-common.labels" . | nindent 4 }}
spec:
  {{- if not (.Values.autoscaling | default dict).enabled }}
  replicas: {{ .Values.replicaCount }}
  {{- end }}
  selector:
    matchLabels:
      {{- include "toolkit-common.selectorLabels" . | nindent 6 }}
  template:
    metadata:
      labels:
        {{- include "toolkit-common.selectorLabels" . | nindent 8 }}
        {{- with .Values.podLabels }}
        {{- toYaml . | nindent 8 }}
        {{- end }}
      annotations:
        {{- if .Values.config.data }}
        checksum/config: {{ include "toolkit-common.configmap" . | sha256sum }}
        {{- end }}
        {{- if $renderedSecrets }}
        checksum/secret: {{ $renderedSecrets | sha256sum }}
        {{- end }}
        {{- with .Values.podAnnotations }}
        {{- toYaml . | nindent 8 }}
        {{- end }}
    spec:
      serviceAccountName: {{ include "toolkit-common.serviceAccountName" . }}
      {{- with .Values.imagePullSecrets }}
      imagePullSecrets:
        {{- toYaml . | nindent 8 }}
      {{- end }}
      {{- with .Values.podSecurityContext }}
      securityContext:
        {{- toYaml . | nindent 8 }}
      {{- end }}
      containers:
        - name: {{ .Chart.Name }}
          image: "{{ with $registry }}{{ . }}/{{ end }}{{ .Values.image.repository }}:{{ .Values.image.tag | default .Chart.AppVersion }}"
          imagePullPolicy: {{ .Values.image.pullPolicy }}
          {{- with .Values.command }}
          command:
            {{- toYaml . | nindent 12 }}
          {{- end }}
          {{- with .Values.args }}
          args:
            {{- toYaml . | nindent 12 }}
          {{- end }}
          {{- with .Values.containerSecurityContext }}
          securityContext:
            {{- toYaml . | nindent 12 }}
          {{- end }}
          ports:
            - name: http
              containerPort: {{ .Values.service.port }}
              protocol: TCP
            {{- if .Values.grpc.enabled }}
            - name: grpc
              containerPort: {{ .Values.grpc.port }}
              protocol: TCP
            {{- end }}
          env:
            - name: POD_NAME
              valueFrom:
                fieldRef:
                  fieldPath: metadata.name
            - name: POD_NAMESPACE
              valueFrom:
                fieldRef:
                  fieldPath: metadata.namespace
            {{- if $otel.enabled }}
            - name: APP__OPENTELEMETRY__EXPORTER__KIND
              value: {{ default "otlp_grpc" $otel.exporterKind | quote }}
            - name: APP__OPENTELEMETRY__EXPORTER__ENDPOINT
              value: {{ required "global.opentelemetry.endpoint is required when enabled" $otel.endpoint | quote }}
            - name: APP__OPENTELEMETRY__TRACING__ENABLED
              value: "true"
            - name: APP__OPENTELEMETRY__METRICS__ENABLED
              value: "true"
            - name: APP__OPENTELEMETRY__RESOURCE__SERVICE_NAME
              value: {{ include "toolkit-common.fullname" $root | quote }}
            {{- end }}
            {{- if .Values.directoryEndpoint }}
            - name: TOOLKIT_DIRECTORY_ENDPOINT
              value: {{ .Values.directoryEndpoint | quote }}
            {{- end }}
            {{- with .Values.extraEnv }}
            {{- toYaml . | nindent 12 }}
            {{- end }}
          {{- with .Values.extraEnvFrom }}
          envFrom:
            {{- toYaml . | nindent 12 }}
          {{- end }}
          volumeMounts:
            {{- if .Values.config.data }}
            - name: config
              mountPath: {{ .Values.config.mountPath }}
              subPath: {{ .Values.config.fileName }}
              readOnly: true
            {{- end }}
            {{- if .Values.saToken.enabled }}
            - name: toolkit-internal-token
              mountPath: {{ .Values.saToken.mountPath }}
              readOnly: true
            {{- end }}
            {{- with .Values.extraVolumeMounts }}
            {{- toYaml . | nindent 12 }}
            {{- end }}
          livenessProbe:
            httpGet:
              path: {{ .Values.probes.liveness.path }}
              port: http
            initialDelaySeconds: {{ .Values.probes.liveness.initialDelaySeconds }}
            periodSeconds: {{ .Values.probes.liveness.periodSeconds }}
            failureThreshold: {{ .Values.probes.liveness.failureThreshold }}
          readinessProbe:
            httpGet:
              path: {{ .Values.probes.readiness.path }}
              port: http
            initialDelaySeconds: {{ .Values.probes.readiness.initialDelaySeconds }}
            periodSeconds: {{ .Values.probes.readiness.periodSeconds }}
            failureThreshold: {{ .Values.probes.readiness.failureThreshold }}
          resources:
            {{- toYaml .Values.resources | nindent 12 }}
      volumes:
        {{- if .Values.config.data }}
        - name: config
          configMap:
            name: {{ include "toolkit-common.fullname" . }}
        {{- end }}
        {{- if .Values.saToken.enabled }}
        - name: toolkit-internal-token
          projected:
            sources:
              - serviceAccountToken:
                  path: {{ .Values.saToken.fileName }}
                  audience: {{ .Values.saToken.audience }}
                  expirationSeconds: {{ .Values.saToken.expirationSeconds }}
        {{- end }}
        {{- with .Values.extraVolumes }}
        {{- toYaml . | nindent 8 }}
        {{- end }}
      {{- with .Values.nodeSelector }}
      nodeSelector:
        {{- toYaml . | nindent 8 }}
      {{- end }}
      {{- with .Values.affinity }}
      affinity:
        {{- toYaml . | nindent 8 }}
      {{- end }}
      {{- with .Values.tolerations }}
      tolerations:
        {{- toYaml . | nindent 8 }}
      {{- end }}
{{- end -}}
