# thewiki Helm Chart

This chart deploys thewiki on Kubernetes with SQLite storage by default.

```sh
helm install thewiki charts/thewiki
```

The default install creates one Deployment, one ClusterIP Service, and one PVC
mounted at `/data` for `sqlite:///data/thewiki.db?mode=rwc`, so a fresh PVC
creates the SQLite file on first boot.

The chart defaults to `ghcr.io/i-doll/thewiki:edge`. Dispatch the Docker
workflow for `main` or override `image.repository` / `image.tag` for local
testing until a release image is published.

## Optional Postgres

The chart can also install the bundled Bitnami Postgres dependency:

```sh
helm install thewiki charts/thewiki \
  --set postgresql.enabled=true
```

The current app image still supports SQLite only. Once the M1 Postgres adapter
lands, provide `database.url` or `database.existingSecret` to point the app at
the bundled or managed Postgres service.

## Optional MinIO

Enable the bundled Bitnami MinIO dependency and S3-compatible storage config:

```sh
helm install thewiki charts/thewiki \
  --set storage.s3.enabled=true \
  --set minio.enabled=true
```

When bundled MinIO is enabled, the app reads the generated MinIO credential
secret directly. For managed object storage, keep `minio.enabled=false` and set
`storage.s3.endpointUrl` plus either `storage.s3.existingSecret` or the
`storage.s3.accessKeyId` / `storage.s3.secretAccessKey` values.

The vendored Bitnami chart is pinned to the newest OCI chart Helm can pull at
this update, but the chart's bundled server image predates the MinIO
`RELEASE.2025-12-20T04-58-37Z` security fix line. The default values override
that server image to Bitnami Public ECR `2026.5.12-debian-12-r1` pinned by
digest and disable the optional MinIO console until the dependency chart can be
refreshed.
