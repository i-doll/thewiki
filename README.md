# thewiki

> **Status: pre-alpha — no code yet.** This repository holds the design and roadmap. Implementation tracking lives in [Issues](https://github.com/i-doll/thewiki/issues) and the [project board](https://github.com/i-doll/thewiki/projects).

A self-hosted, single-binary wiki for public reference use. Aims to be **simpler to operate than MediaWiki** while matching **Wiki.js** for document-format breadth.

## Goals

- **Single static binary** — `./thewiki` and you're running. Docker, Kubernetes (Helm), and bare-metal deploys all supported.
- **Bring-your-own database** — SQLite/libsql or Postgres. Pick what fits.
- **Bring-your-own storage** — S3, R2, MinIO, or in-database blob storage. No required external services.
- **Pluggable content renderers** — Markdown at v1; AsciiDoc, MediaWiki wikitext, reStructuredText, and others post-v1 behind a stable `Renderer` trait.
- **Configurable auth** — anonymous editing on/off, registration open/closed/invite, edit-approval workflow, per-page protection. The admin picks the model at install time.
- **Full revision history** — diffs, reverts, recent-changes feed, audit log — all the table-stakes wiki affordances.
- **Modern editor** — Tiptap WYSIWYG by default with a CodeMirror source-mode toggle for power users.

## Tech stack

| Layer | Choice |
|---|---|
| Language | Rust |
| Web framework | Axum + Tower |
| Database | sqlx (SQLite, libsql, Postgres) |
| Search | Tantivy (embedded) |
| Object storage | `object_store` crate (S3-compatible) |
| API | REST (OpenAPI via utoipa) + GraphQL (async-graphql) |
| Frontend | TanStack Start (SPA) on React + Vite, TanStack Router + Query |
| Editor | Tiptap + CodeMirror 6 |
| License | [AGPL-3.0](./LICENSE) |

For the full picture — crate layout, the `Renderer` trait, the database story, the frontend split, and how it all fits together — see [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md).

## Container image

Multi-arch images (`linux/amd64`, `linux/arm64`) are published to GHCR at [`ghcr.io/i-doll/thewiki`](https://github.com/i-doll/thewiki/pkgs/container/thewiki). The `:edge` tag tracks `main`; release tags (`v0.1.0`, etc.) follow semver and additionally publish `:latest`.

```sh
docker run --rm -p 8080:8080 ghcr.io/i-doll/thewiki:edge
```

The image is built on `gcr.io/distroless/cc-debian12:nonroot` — non-root by default (uid `65532`), no shell, no package manager. The Rust binary lives at `/usr/local/bin/thewiki` and the built frontend at `/srv/web/dist/` (the latter is staged for [#16](https://github.com/i-doll/thewiki/issues/16), where `rust-embed` will pick it up). The server listens on `0.0.0.0:8080`; probe `GET /healthz` for liveness.

## Roadmap

- **M0 — Walking skeleton**: single binary boots, SQLite backend, Markdown CRUD with history/diff/revert.
- **M1 — First publishable wiki**: Postgres + libsql, search, namespaces, categories, wikilinks, media uploads, audit log.
- **M2 — Moderation & power features**: edit approval queue, talk pages, templates with transclusion, admin UI.
- **Post-v1**: AsciiDoc (asciidork), wikitext/reST, federation, theming.

See [open milestones](https://github.com/i-doll/thewiki/milestones).

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) and our [Code of Conduct](./CODE_OF_CONDUCT.md).

## Security

To report a security issue, see [SECURITY.md](./SECURITY.md).
