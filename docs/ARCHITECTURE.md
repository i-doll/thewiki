# Architecture

This document gives contributors a single-page tour of how `thewiki` is laid out and the reasoning behind the major choices. It is meant to be enough to find your way around the tree and to understand why a given concern lives where it does. Detailed design decisions live in [Architecture Decision Records](./adr/) and are linked inline.

Status: **pre-alpha**. The layout described here is the target state for M0 (the walking skeleton). Where something does not yet exist on disk, this document calls it out explicitly.

## 1. Goals

`thewiki` is a self-hosted wiki for public reference use, with two positioning goals:

1. **Simpler to operate than MediaWiki.** One static binary, one optional database, no PHP, no required external services. `./thewiki` and you are running.
2. **As broad as Wiki.js on document formats.** Markdown ships at v1. AsciiDoc, MediaWiki wikitext, reStructuredText, and others plug in post-v1 behind a stable `Renderer` trait without rewriting the page pipeline.

Concrete non-goals for v1: federation, AsciiDoc, theming, mobile clients. These are tracked under the `Post-v1` milestone.

Everything else in this document follows from those two goals. When in doubt, optimise for **operator ergonomics** (one binary, sane defaults, configurable auth) and **renderer-pluggability** (no Markdown assumptions leaking past the `core` crate).

## 2. Top-level layout

```
thewiki/
├── crates/
│   ├── api/          # `thewiki-api` — Axum HTTP server, REST + GraphQL handlers, middleware (binary)
│   ├── core/         # `thewiki-core` — Domain model + traits (Renderer, Repository, ...)
│   ├── render/       # `thewiki-render` — Renderer implementations (Markdown at M0)
│   ├── search/       # `thewiki-search` — Tantivy index + query layer (M1)
│   └── storage/      # `thewiki-storage` — sqlx-based persistence + object_store glue
├── xtask/            # Repo-local automation (`cargo xtask <command>`); not published
├── web/              # React SPA: TanStack Router + TanStack Query + Vite, builds to dist/
├── migrations/       # sqlx migrations (per-backend subdirs as needed)
├── docs/
│   ├── ARCHITECTURE.md
│   └── adr/          # Architecture Decision Records
├── charts/
│   └── thewiki/      # Helm chart (M1+)
├── .github/          # Issue templates, PR template, workflows
├── Cargo.toml        # Workspace manifest
├── README.md
├── CONTRIBUTING.md
├── CODE_OF_CONDUCT.md
├── SECURITY.md
└── LICENSE           # AGPL-3.0
```

Crate packages on disk are prefixed `thewiki-*` (e.g. `crates/api/Cargo.toml` declares `name = "thewiki-api"`) to keep crates.io names unambiguous if we ever publish. The directory names stay short for ergonomics. The `xtask` crate follows the cargo-xtask convention for repo-local automation and is not part of the release artefacts.

The Rust workspace lives at the repo root. The frontend is a sibling tree under `/web` rather than a Cargo member, because its build tooling (pnpm + Vite) is independent and its output (`web/dist/`) is consumed by the Rust binary at compile time via `rust-embed`.

## 3. Crate responsibilities and dependency direction

The workspace splits along **stable seams**: things that have to be replaceable (renderer, storage backend, search) live behind traits in `core`, and concrete implementations live in dedicated crates that depend on `core`.

| Crate              | Owns                                                              |
|--------------------|-------------------------------------------------------------------|
| `thewiki-core`     | Domain types (`Page`, `Revision`, `User`, `Role`, `Namespace`), traits (`Renderer`, `Repository`, `SearchIndex`, `ObjectStore`-wrapper), error types, shared value objects. No I/O. No framework dependencies. |
| `thewiki-render`   | `Renderer` implementations. M0: Markdown (the crate chosen by [ADR-0001](./adr/0001-markdown-renderer.md)). Post-v1: AsciiDoc, wikitext, reST. |
| `thewiki-storage`  | sqlx connection pools, `Repository` implementations per backend (SQLite at M0; libsql, Postgres at M1), migration runner, `object_store` integration. |
| `thewiki-search`   | Tantivy index management, async indexing hook fired on revision commit, query DSL. M1. |
| `thewiki-api`      | Axum app construction, REST handlers (utoipa-annotated), GraphQL schema (async-graphql, M1), auth middleware, session handling, `rust-embed` mounting of the SPA. Wires everything together. The binary's `main` lives here. |
| `xtask`            | Repo-local automation (migrations, codegen, release tasks). Invoked as `cargo xtask <command>`. Not published. |

### Dependency direction

```
              ┌──────────────────────────┐
              │           api            │
              └─────┬──────┬───────┬─────┘
                    │      │       │
            ┌───────▼──┐ ┌─▼─────┐ ┌▼────────┐
            │ storage  │ │render │ │ search  │
            └───────┬──┘ └─┬─────┘ └┬────────┘
                    │      │        │
                    └──────▼────────┘
                           │
                       ┌───▼───┐
                       │ core  │
                       └───────┘
```

Rules:

- **`core` depends on nothing internal.** It compiles standalone.
- Every other internal crate depends on `core` and **only** `core`.
- `api` is the only crate that depends on `storage`, `render`, and `search` simultaneously. It is the composition root.
- **No cyclic dependencies.** If you find yourself wanting `core` to depend on `storage`, you have either misplaced a type or you need a new trait in `core` that `storage` implements.

This is enforced by code review for now. A `cargo deny` check or a CI script can mechanise it later if it becomes an issue.

## 4. The `Renderer` trait

Document-format breadth is a v1 positioning goal, but Markdown is the only format that ships at v1. The seam is a trait in `core`:

```rust
// crates/core/src/render.rs (sketch)
pub trait Renderer: Send + Sync {
    /// Identifier used in storage and in the `content_format` column.
    fn format(&self) -> ContentFormat;

    /// Render source to safe HTML. Implementations are responsible for
    /// sanitisation; the API layer does not post-process.
    fn render(&self, source: &str, ctx: &RenderContext) -> Result<RenderedHtml, RenderError>;

    /// Extract outgoing wiki-links for backlink + redlink calculation.
    fn extract_links(&self, source: &str) -> Vec<WikiLink>;
}
```

`RenderContext` carries the current namespace, the page slug, and a `LinkResolver` so the renderer can decide which `[[WikiLink]]`s are red (M1).

Why a trait, not an enum:

- Adding AsciiDoc post-v1 must not touch `core` or `api`.
- Third parties can plug in a renderer crate without forking.
- Renderers can carry their own state (parser instances, cached options) without leaking into the domain model.

The M0 implementation lives in `crates/render/src/markdown.rs`. The choice between `pulldown-cmark` and `comrak` is the subject of **ADR-0001** (see `docs/adr/0001-markdown-renderer.md`, tracked by issue #2).

## 5. Database story

### Backends

`thewiki` targets three SQL backends through a single query layer:

| Backend  | Milestone | Use case                                         |
|----------|-----------|--------------------------------------------------|
| SQLite   | M0        | Default single-binary deploys, tests, dev.       |
| libsql   | M1        | Remote/embedded with libsql-server, Turso users. |
| Postgres | M1        | Larger production deploys, multi-writer.         |

### Query layer

We use [`sqlx`](https://github.com/launchbadge/sqlx) with compile-time-checked queries. The repository pattern keeps SQL inside `storage`:

```rust
// crates/core/src/repo.rs (sketch)
#[async_trait]
pub trait PageRepository: Send + Sync {
    async fn get(&self, ns: NamespaceId, slug: &str) -> Result<Option<Page>, RepoError>;
    async fn upsert(&self, page: NewPage) -> Result<Page, RepoError>;
    async fn list(&self, ns: NamespaceId, page: Pagination) -> Result<Vec<Page>, RepoError>;
    // ...
}
```

`storage` provides one impl per backend, behind the same trait. `api` holds an `Arc<dyn PageRepository>` (and friends) in app state and is backend-agnostic at the call site. Three repositories at M0: `PageRepository`, `RevisionRepository`, `UserRepository`. More land alongside the features that need them.

### Migrations

Migrations live in `/migrations/`. We use `sqlx migrate` for now; the choice between `sqlx-cli` and `refinery` is open and will be decided as part of issue #5. SQLite is the only backend at M0; Postgres and libsql will get backend-specific subdirs (`migrations/postgres/`, `migrations/libsql/`) only if dialect divergence forces it. The aim is a single migration set whenever the dialects let us.

## 6. Search

Search is **embedded**. We use [Tantivy](https://github.com/quickwit-oss/tantivy), which gives us a Lucene-class FTS index without an external service.

Lifecycle:

- The index lives on local disk (path configurable; defaults next to the SQLite file).
- On every revision commit, `storage` emits an event that `search` consumes asynchronously to update the index. Indexing failures do not fail the write.
- The query layer exposes both REST and (M1) GraphQL endpoints.

This lands in M1 (issue #26+). M0 has no search.

## 7. Object storage

User uploads (images, attachments) go through the [`object_store`](https://docs.rs/object_store/) crate. That gives us a single API and pluggable backends:

- S3 and S3-compatible (Cloudflare R2, MinIO, Backblaze B2, etc.)
- In-database BLOB column as a fallback for small deploys that do not want a separate bucket.

The active backend is chosen by config. `storage` owns the integration; `api` deals only in opaque `MediaId`s. M1.

## 8. Frontend split

The frontend is a **separate React SPA** under `/web` (TanStack Router for type-safe file-based routing, TanStack Query for API data, Vite for build, Tailwind v4 for styling):

- React + Vite + TypeScript.
- TanStack Router for type-safe routes, TanStack Query for server state.
- Tiptap WYSIWYG editor with a CodeMirror 6 source-mode toggle (per [issue #17](https://github.com/i-doll/thewiki/issues/17)).
- Builds to `web/dist/` via `pnpm build`.

The Rust binary embeds the built SPA via [`rust-embed`](https://docs.rs/rust-embed/) at compile time. There is **one deploy artefact**: a single static binary that serves both the API and the SPA. No separate `nginx` or static-host step.

In dev, the SPA runs against Vite's dev server and proxies API calls to `cargo run`. In CI and release builds, `pnpm build` runs first, then `cargo build --release` picks up the assets.

The API surface is **REST + GraphQL**:

- REST is the primary surface, documented via [`utoipa`](https://docs.rs/utoipa/) (OpenAPI 3) with Swagger UI at `/api/docs` (M1).
- GraphQL via [`async-graphql`](https://docs.rs/async-graphql/) mirrors the REST coverage and lands in M1. The SPA can use either; the public API contract is REST.

## 9. Auth model

The auth model is **role-based and configurable at install**. The administrator picks defaults in `thewiki.toml`; nothing is hard-coded.

Configurable axes:

- **Anonymous editing**: on / off.
- **Registration**: open / invite-only / closed.
- **Approval queue**: edits from new or anonymous users land in a pending queue (M2 feature; toggle exists from M0).
- **Per-page protection** (M1): edit / move / delete each require a minimum role.

Implementation notes:

- Sessions are stored in the database (no JWT) so logout, revocation, and "log out all sessions" actually work.
- Passwords are hashed with [`argon2`](https://docs.rs/argon2/) (argon2id, sensible defaults).
- Role checks are Axum middleware. Roles are simple at v1: `anonymous`, `user`, `editor`, `moderator`, `admin`. Per-page ACLs (M1) layer on top.

OAuth, OIDC, SAML, and LDAP are explicitly post-M2.

## 10. Configuration

Configuration loads via [`figment`](https://docs.rs/figment/) with two layered sources:

1. A TOML file at `thewiki.toml` (path overridable via `--config` or `THEWIKI_CONFIG`).
2. Environment variables prefixed `THEWIKI_` (e.g. `THEWIKI_DATABASE_URL`, `THEWIKI_AUTH_ALLOW_ANONYMOUS`).

Environment overrides the file. Both override built-in defaults. Config is read once at startup and held in `Arc<Config>` in app state; reloading is a v2 problem.

There is no separate `secrets.toml`. Secrets come from env in production deploys (12-factor); the file is for everything else.

## 11. Deploy artefacts

`thewiki` ships in three forms:

| Artefact      | Built by          | When |
|---------------|-------------------|------|
| Static binary | `cargo build --release`, cross-compiled for linux-gnu/linux-musl/macos/windows | M0 (issue #21) |
| Docker image  | `Dockerfile` + GHCR publish workflow | M0 (issue #22) |
| Helm chart    | `charts/thewiki/` | M1 (issue #39) |

The Docker image is a thin wrapper around the static binary (distroless or alpine, TBD in issue #22). The Helm chart is the K8s deploy path; we are not planning an operator.

Cloudflare Workers as a deploy target was considered and dropped — it forced too many concessions away from idiomatic Rust.

## 12. ADR convention

Architecture Decision Records live in `/docs/adr/`. The format is the lightweight [MADR](https://adr.github.io/madr/) style.

- Filename: `NNNN-kebab-case-title.md`, monotonically numbered from `0001`.
- One decision per ADR. Status: `Proposed` → `Accepted` → optionally `Superseded by ADR-NNNN`.
- Once accepted, an ADR is **append-only**. Changing direction means writing a new ADR that supersedes it.

Write an ADR when:

- You are picking between named alternatives where the choice affects multiple crates (e.g. ADR-0001: Markdown renderer).
- You are locking in a public interface or wire format that contributors will need to argue with later (e.g. the upcoming template-syntax ADR in M2).
- You are reversing a previously accepted decision.

You **do not** need an ADR for routine implementation choices, refactors, or library bumps. If you are not sure, lean toward writing one — they are cheap.

Currently planned:

- **ADR-0001 — Markdown renderer** (`pulldown-cmark` vs `comrak`), tracked by [issue #2](https://github.com/i-doll/thewiki/issues/2). Lands during M0.
- **ADR-0002 — Template syntax**, tracked by [issue #44](https://github.com/i-doll/thewiki/issues/44). Lands during M2 alongside transclusion.

## 13. Out of scope (post-v1)

To keep v1 shippable, the following are explicitly **not** v1 work and live under the `Post-v1` milestone:

- **Federation** (ActivityPub or otherwise). Interesting; not a v1 problem.
- **AsciiDoc rendering.** No mature pure-Rust AsciiDoc parser exists yet; we are watching the `asciidork` project. The `Renderer` trait is designed so this is purely additive when the time comes.
- **Theming.** v1 ships one well-considered theme. Per-instance theming is a v2 conversation.
- **Mobile clients.** The SPA is responsive; native clients are not on the roadmap.

If you have an itch in any of these areas, file an issue against the `Post-v1` milestone so it is tracked but does not pull focus from the walking skeleton.

---

## Where to go next

- **Roadmap and open work**: [GitHub milestones](https://github.com/i-doll/thewiki/milestones).
- **Contributing**: [CONTRIBUTING.md](../CONTRIBUTING.md).
- **Decisions**: [docs/adr/](./adr/).
- **Reporting security issues**: [SECURITY.md](../SECURITY.md).
