{{/*
Naming helpers.

Every object is named `<release>-<database>-<role>`, so a release called `myota` in
namespace `myota-test` produces `myota-geo-postgres` — the name the issue's Verify
command uses. Keeping the release name fixed across environments is what makes the two
namespaces differ only by their contents rather than by their naming scheme.
*/}}

{{- define "myota.postgresName" -}}
{{- printf "%s-%s-postgres" .root.Release.Name .name -}}
{{- end -}}

{{- define "myota.pgbouncerName" -}}
{{- printf "%s-%s-pgbouncer" .root.Release.Name .name -}}
{{- end -}}

{{- define "myota.dbCredentialsSecret" -}}
{{- printf "%s-%s-db-credentials" .root.Release.Name .name -}}
{{- end -}}

{{/*
The only host a service should ever use. Nothing in this chart puts a `-postgres` host
into a ConfigMap a workload reads; the pooler is the sole application path, and
`tools/check_deploy.py` fails if that changes.
*/}}
{{- define "myota.serviceDatabaseURL" -}}
{{- printf "postgresql://%s@%s:6432/%s" .root.Values.postgres.appUser (include "myota.pgbouncerName" .) .database -}}
{{- end -}}

{{- define "myota.labels" -}}
app.kubernetes.io/part-of: myota
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}
