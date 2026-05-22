{{/*
Expand the chart name.
*/}}
{{- define "thewiki.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "thewiki.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/*
Chart label value.
*/}}
{{- define "thewiki.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Common labels.
*/}}
{{- define "thewiki.labels" -}}
helm.sh/chart: {{ include "thewiki.chart" . }}
{{ include "thewiki.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{/*
Selector labels.
*/}}
{{- define "thewiki.selectorLabels" -}}
app.kubernetes.io/name: {{ include "thewiki.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/*
Service account name.
*/}}
{{- define "thewiki.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "thewiki.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{/*
Name of the secret containing THEWIKI_DATABASE__URL.
*/}}
{{- define "thewiki.databaseSecretName" -}}
{{- default (printf "%s-database" (include "thewiki.fullname" .)) .Values.database.existingSecret -}}
{{- end -}}

{{/*
Build the database URL when the operator did not provide one directly.
*/}}
{{- define "thewiki.databaseUrl" -}}
{{- if .Values.database.url -}}
{{- .Values.database.url -}}
{{- else -}}
{{- if not .Values.persistence.enabled -}}
{{- fail "persistence.enabled must be true for the default SQLite database, or set database.url/database.existingSecret" -}}
{{- end -}}
{{- $mountPath := required "persistence.mountPath is required for the default SQLite database" .Values.persistence.mountPath -}}
{{- printf "sqlite://%s/thewiki.db?mode=rwc" $mountPath -}}
{{- end -}}
{{- end -}}

{{/*
Name of the secret containing S3 credentials.
*/}}
{{- define "thewiki.storageSecretName" -}}
{{- default (printf "%s-storage" (include "thewiki.fullname" .)) .Values.storage.s3.existingSecret -}}
{{- end -}}

{{/*
Name of the bundled Bitnami MinIO release resources.
*/}}
{{- define "thewiki.minioFullname" -}}
{{- default (printf "%s-minio" .Release.Name) .Values.minio.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Name of the Bitnami MinIO credential secret.
*/}}
{{- define "thewiki.minioSecretName" -}}
{{- if .Values.minio.auth.existingSecret -}}
{{- .Values.minio.auth.existingSecret -}}
{{- else -}}
{{- include "thewiki.minioFullname" . -}}
{{- end -}}
{{- end -}}

{{/*
Best-effort MinIO service name for the bundled Bitnami dependency.
*/}}
{{- define "thewiki.minioHost" -}}
{{- include "thewiki.minioFullname" . -}}
{{- end -}}

{{/*
Resolve the S3 endpoint URL.
*/}}
{{- define "thewiki.s3EndpointUrl" -}}
{{- if .Values.storage.s3.endpointUrl -}}
{{- .Values.storage.s3.endpointUrl -}}
{{- else if .Values.minio.enabled -}}
{{- printf "http://%s:9000" (include "thewiki.minioHost" .) -}}
{{- end -}}
{{- end -}}
