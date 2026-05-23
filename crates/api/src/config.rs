//! Layered configuration loader for the `thewiki` binary.
//!
//! Loading order (later layers override earlier ones):
//!
//! 1. [`Config::defaults`] — built-in sane defaults baked into the binary.
//! 2. A TOML file (typically `thewiki.toml`), loaded only when an explicit path
//!    is supplied. Missing-but-requested is a hard error; the operator asked
//!    for it, so silently falling back would be a footgun.
//! 3. Environment variables prefixed `THEWIKI_`. Nested keys use a double
//!    underscore separator: `THEWIKI_SERVER__BIND` overrides `server.bind`,
//!    `THEWIKI_AUTH__ARGON2__MEMORY_KIB` overrides `auth.argon2.memory_kib`.
//!
//! The double-underscore separator is a deliberate choice — single underscores
//! collide with snake_case field names (e.g. `acquire_timeout_secs`).
//!
//! Configuration is read once at startup; reloading is a v2 concern.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize};

/// Top-level configuration, populated by merging the layered providers.
///
/// Every field has a default so a brand-new deploy with no config file and no
/// environment overrides still boots successfully.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// HTTP server (bind address, timeouts, runtime sizing).
    pub server: ServerConfig,
    /// Persistence layer (database URL + pool tuning).
    pub database: DatabaseConfig,
    /// Object storage backend (DB blobs by default, S3-compatible at M1).
    pub storage: StorageConfig,
    /// Auth model defaults (anonymous edits, registration, hashing parameters).
    pub auth: AuthConfig,
    /// Full-text search (Tantivy) index location and commit cadence.
    pub search: SearchConfig,
    /// Abuse protection for public endpoints.
    pub rate_limit: RateLimitConfig,
    /// Administrative audit-log retention.
    pub audit_log: AuditLogConfig,
    /// Observability (log format and filter).
    pub telemetry: TelemetryConfig,
    /// GraphQL surface (#37). Enabled by default; the playground and
    /// introspection knobs typically get flipped off in production via env.
    #[serde(default)]
    pub graphql: GraphQLConfig,
    /// CAPTCHA provider configuration (#41). Defaults to the noop provider,
    /// so a fresh deploy doesn't require an hCaptcha account just to boot.
    #[serde(default)]
    pub captcha: CaptchaConfig,
    /// Security policy: X-Forwarded-For trust and blocklist plumbing (#42).
    #[serde(default)]
    pub security: SecurityConfig,
    /// Renderer tuning (#45 templates, future Markdown knobs).
    #[serde(default)]
    pub render: RenderConfig,
    /// Moderation policy — drives the edit approval queue (#40).
    #[serde(default)]
    pub moderation: ModerationConfig,
}

/// Renderer tuning. Today exposes only the template subsection (#45); other
/// renderer knobs (Markdown options, smart-punctuation overrides, ...) plug
/// in here as they ship.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderConfig {
    /// Template transclusion engine (#45).
    #[serde(default)]
    pub template: TemplateConfig,
}

/// Template transclusion (#45). The depth cap defends against pathological
/// templates that expand without bound — both deeply nested chains and
/// recursive self-references. Cycle detection is independent and fires
/// before the depth counter (see ADR-0002).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateConfig {
    /// Hard cap on transclusion depth. Defaults to 20 (per ADR-0002).
    ///
    /// Exceeding this cap emits a `[template error: recursion limit
    /// exceeded]` inline diagnostic at the originating call site.
    #[serde(default = "default_max_recursion_depth")]
    pub max_recursion_depth: u32,
}

/// `serde` default for [`TemplateConfig::max_recursion_depth`].
fn default_max_recursion_depth() -> u32 {
    20
}

impl Default for TemplateConfig {
    fn default() -> Self {
        Self {
            max_recursion_depth: default_max_recursion_depth(),
        }
    }
}

/// CAPTCHA provider configuration (#41).
///
/// Operators flip `provider = "hcaptcha"` and supply both keys when they
/// expose the wiki publicly; the noop default is the right call for
/// private deployments where the auth wall is already enough. The
/// `apply_to_*` flags decide which surfaces consult the provider — both
/// land in this struct so they can be toggled independently of the
/// provider choice (e.g. require CAPTCHA on registration but not on
/// anonymous edits when the latter are also moderated through the
/// approval queue).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptchaConfig {
    /// Which provider to instantiate. `"noop"` (the default) skips
    /// verification entirely; `"hcaptcha"` POSTs to the upstream verifier.
    #[serde(default)]
    pub provider: CaptchaProviderKind,
    /// Public site key handed to the rendered widget. Required when
    /// `provider = "hcaptcha"`; ignored otherwise.
    #[serde(default)]
    pub site_key: String,
    /// Server-side secret. Required when `provider = "hcaptcha"`; never
    /// emitted to the SPA.
    #[serde(default)]
    pub secret_key: String,
    /// When `true`, the registration handler requires a non-empty
    /// `captcha_response` field and runs it through the provider before
    /// creating the user. Defaults to `true` — the most common attack
    /// surface for a public wiki.
    #[serde(default = "default_captcha_apply_to_registration")]
    pub apply_to_registration: bool,
    /// When `true`, anonymous edits (the configurable-auth fallback that
    /// fires when `auth.anonymous_edits = true`) also require a verified
    /// CAPTCHA token. Defaults to `false` — operators who want
    /// anonymous edits at all usually pair the flag with the approval
    /// queue (`auth.approval_required_for = "anonymous"`) which is the
    /// stronger gate.
    ///
    /// TODO(#41-followup): this field is currently **inert**. There is no
    /// anonymous-edit code path yet — every page create/update goes
    /// through `EditorExtractor`, which requires an `AuthSession`. Once
    /// the SPA ships an anonymous-edit affordance we'll thread
    /// `captcha_response` through `CreatePageRequest` / `UpdatePageRequest`
    /// and consult this flag from the page handlers. Until then, flipping
    /// it has no observable effect.
    #[serde(default)]
    pub apply_to_anonymous_edits: bool,
}

/// `serde` default for [`CaptchaConfig::apply_to_registration`].
fn default_captcha_apply_to_registration() -> bool {
    true
}

impl Default for CaptchaConfig {
    fn default() -> Self {
        Self {
            provider: CaptchaProviderKind::default(),
            site_key: String::new(),
            secret_key: String::new(),
            apply_to_registration: default_captcha_apply_to_registration(),
            apply_to_anonymous_edits: false,
        }
    }
}

/// Which CAPTCHA implementation the API should wire up at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum CaptchaProviderKind {
    /// No-op provider that accepts every token. Used as the default and
    /// in private deploys where the auth wall is the protection.
    #[default]
    Noop,
    /// hCaptcha (https://www.hcaptcha.com/). Verifies tokens against
    /// `https://api.hcaptcha.com/siteverify`.
    Hcaptcha,
}

/// Security policy — X-Forwarded-For trust and the runtime hooks for the
/// blocklist subsystem (#42).
///
/// The reverse-proxy story here is intentionally separate from the
/// rate-limit one ([`RateLimitConfig::client_ip_header`]): the blocklist
/// runs *before* auth and rate limiting, so it owns its own resolution
/// path. Operators who terminate TLS behind a single proxy will usually
/// configure both blocks identically; we keep them split so a deploy that
/// only trusts XFF for blocklisting (and not for rate limiting) is
/// expressible.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityConfig {
    /// When `true`, the blocklist middleware honours `X-Forwarded-For`
    /// from upstream peers in `trusted_proxies`. Without this flag the
    /// middleware always uses the socket peer, which is the safe default
    /// for direct-bind deploys.
    #[serde(default)]
    pub trust_x_forwarded_for: bool,
    /// CIDRs of upstream proxies whose `X-Forwarded-For` header is
    /// honoured. The middleware walks the chain right-to-left and selects
    /// the first IP that is not inside any of these CIDRs — i.e. it
    /// strips trusted hops to find the perceived client IP.
    ///
    /// Stored as strings in the wire form (`["10.0.0.0/8", "::1/128"]`)
    /// so the operator can drop them straight into `thewiki.toml` without
    /// custom serde. The middleware parses them once on boot via
    /// [`ipnet::IpNet`](https://docs.rs/ipnet) and caches the result.
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
}

/// GraphQL endpoint configuration.
///
/// Mirrors the operator-facing knobs documented in `thewiki.example.toml`.
/// All defaults match the "developer-friendly" posture: GraphiQL and
/// introspection are on, persisted queries are off (they're an
/// opt-in surface today), and the depth/complexity limits are generous
/// enough that a hand-written query won't trip them but a hostile one
/// will.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphQLConfig {
    /// Serve the GraphiQL HTML at `/api/graphql/playground`. Flip to `false`
    /// in production once clients are wired — there's no auth on the
    /// playground page itself (the queries it sends still hit the same
    /// resolvers and respect the session cookie), but it advertises the
    /// schema, so the safe production posture is "playground off,
    /// introspection off".
    #[serde(default = "default_graphql_playground")]
    pub playground_enabled: bool,
    /// Allow GraphQL introspection (`__schema` / `__type`). Disabling this
    /// hides the schema from unauthenticated callers, which is the standard
    /// production posture. Defaults to `true` so out-of-the-box deploys can
    /// be explored with any GraphQL client.
    #[serde(default = "default_graphql_introspection")]
    pub introspection_enabled: bool,
    /// Accept persisted-query lookups (Apollo APQ shape: clients send
    /// `extensions.persistedQuery.sha256Hash`). When `true` the server
    /// transparently caches queries in-memory the first time it sees them
    /// and answers subsequent hash-only requests from the cache. Defaults to
    /// `false` because the cache is process-local and adds an additional
    /// surface that operators should opt into.
    #[serde(default)]
    pub persisted_queries_enabled: bool,
    /// Cap on query depth. Defends against deeply nested adversarial queries
    /// (the classic `{ user { friends { friends { ... } } } }` exponent).
    #[serde(default = "default_graphql_max_depth")]
    pub max_query_depth: u32,
    /// Cap on query complexity. async-graphql's complexity score adds 1 per
    /// selected field by default; resolvers can override the cost individually.
    #[serde(default = "default_graphql_max_complexity")]
    pub max_query_complexity: u32,
}

fn default_graphql_playground() -> bool {
    true
}
fn default_graphql_introspection() -> bool {
    true
}
fn default_graphql_max_depth() -> u32 {
    15
}
fn default_graphql_max_complexity() -> u32 {
    1_000
}

impl Default for GraphQLConfig {
    fn default() -> Self {
        Self {
            playground_enabled: default_graphql_playground(),
            introspection_enabled: default_graphql_introspection(),
            persisted_queries_enabled: false,
            max_query_depth: default_graphql_max_depth(),
            max_query_complexity: default_graphql_max_complexity(),
        }
    }
}

/// HTTP server tuning.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address the HTTP listener binds to (`host:port`).
    ///
    /// Defaults to `0.0.0.0:8080`. Operators behind a reverse proxy will often
    /// flip this to `127.0.0.1:8080` so the binary only listens on the loopback
    /// interface.
    pub bind: String,

    /// Per-request timeout. Parsed as a humantime duration (e.g. `"30s"`,
    /// `"2m"`). `None` disables the timeout layer entirely — only sensible if
    /// you have a reverse proxy enforcing one.
    #[serde(default)]
    pub request_timeout: Option<String>,

    /// Override Tokio's worker thread count. `None` falls back to the runtime
    /// default (one worker per logical CPU).
    #[serde(default)]
    pub worker_threads: Option<usize>,

    /// Serve the embedded SPA bundle (`web/dist/`) as a fallback for any
    /// request that doesn't match an API route (#16).
    ///
    /// Default: `true`. The single-binary production deploy expects this.
    /// Local frontend developers running `pnpm dev` flip it to `false` so
    /// unmatched routes return 404 — Vite proxies `/api/*` to the Rust
    /// backend and serves the SPA itself.
    #[serde(default = "default_serve_frontend")]
    pub serve_frontend: bool,
}

/// `serde` default for [`ServerConfig::serve_frontend`].
#[must_use]
fn default_serve_frontend() -> bool {
    true
}

/// Persistence layer configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// Which backend the URL targets. Defaults to `sqlite`. The Postgres
    /// adapter (#25) is selected with `driver = "postgres"`; the libsql
    /// adapter lands in #24. The dispatch reading this field is wired in
    /// alongside the runtime that owns it; the field is parsed here so
    /// operators can already declare their target backend.
    #[serde(default)]
    pub driver: DatabaseDriver,
    /// Database URL, in sqlx's URL syntax. SQLite is the only M0 backend;
    /// `postgres://` is supported behind the `postgres` cargo feature
    /// (M1, #25) and `libsql://` lands in #24.
    pub url: String,
    /// Maximum number of pooled connections.
    pub max_connections: u32,
    /// Timeout (seconds) for acquiring a connection from the pool before
    /// returning an error to the caller.
    pub acquire_timeout_secs: u64,
}

/// Database driver selector. `sqlite` is the default and the only backend
/// `thewiki` ships at M0; `postgres` becomes available once the binary is
/// built with `--features thewiki-storage/postgres`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DatabaseDriver {
    /// SQLite via `sqlx::SqlitePool`. URL form: `sqlite://path` or
    /// `sqlite::memory:`.
    #[default]
    Sqlite,
    /// Postgres via `sqlx::PgPool`. URL form: `postgres://user:pass@host/db`.
    Postgres,
}

/// Object storage backend selector.
///
/// `Db` keeps blobs in the primary database (simple, works out of the box).
/// `S3` is selected by setting `backend = { kind = "s3", … }`. The media
/// upload pipeline (#32) consults this struct to pick where blob payloads
/// live; metadata always stays in the primary DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    /// Where blob payloads land — DB row vs S3-compatible bucket.
    pub backend: StorageBackend,
    /// Upload validation tuning: size limit and accepted content types.
    #[serde(default)]
    pub media: MediaConfig,
}

/// Tuning for the [`POST /api/v1/media`](crate) upload endpoint.
///
/// Operators can tighten the content-type allowlist or shrink the size cap
/// without touching code. The defaults match the issue spec — common image
/// types plus a 10 MiB cap.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaConfig {
    /// Hard ceiling on the byte length of any single upload. Requests over
    /// this limit get a 413 Payload Too Large before the blob is persisted.
    ///
    /// Default: 10 MiB.
    #[serde(default = "default_max_upload_bytes")]
    pub max_upload_bytes: u64,
    /// IANA media types the upload endpoint accepts. Anything outside this
    /// set is rejected with 415 Unsupported Media Type.
    ///
    /// SVGs are allowed but sanitised through `ammonia` before storage —
    /// see the `<script>` / `on*` handler scrubbing in the media handler.
    ///
    /// Default: the set listed in #32.
    #[serde(default = "default_allowed_content_types")]
    pub allowed_content_types: Vec<String>,
}

/// `serde` default for [`MediaConfig::max_upload_bytes`].
fn default_max_upload_bytes() -> u64 {
    10 * 1024 * 1024
}

/// `serde` default for [`MediaConfig::allowed_content_types`].
fn default_allowed_content_types() -> Vec<String> {
    vec![
        "image/png".to_string(),
        "image/jpeg".to_string(),
        "image/gif".to_string(),
        "image/webp".to_string(),
        "image/svg+xml".to_string(),
    ]
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            max_upload_bytes: default_max_upload_bytes(),
            allowed_content_types: default_allowed_content_types(),
        }
    }
}

/// Available object-storage backends.
///
/// `#[non_exhaustive]` so adding new backends (e.g. local filesystem) isn't a
/// breaking change for downstream matchers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[non_exhaustive]
pub enum StorageBackend {
    /// Store blobs in the primary database. Default for small deploys.
    Db,
    /// S3-compatible object store (AWS S3, R2, MinIO, Backblaze B2, ...).
    S3 {
        /// Bucket name.
        bucket: String,
        /// AWS region (or region-like value for S3-compatible providers).
        region: String,
        /// Optional custom endpoint URL (for R2, MinIO, etc.). When `None` the
        /// default AWS endpoint for `region` is used.
        #[serde(default)]
        endpoint_url: Option<String>,
    },
}

/// Auth model defaults. Runtime enforcement lives in #14; this struct is the
/// surface operators configure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// When `true`, anonymous (logged-out) visitors can edit pages. Defaults
    /// to `false` for safety.
    pub anonymous_edits: bool,
    /// Account registration policy.
    pub registration: RegistrationPolicy,
    /// Which classes of users land in the moderator approval queue.
    pub approval_required_for: ApprovalScope,
    /// Session lifetime in hours. Sessions are stored in the database (#9), so
    /// shortening this is the right knob if you want forced re-auth.
    pub session_ttl_hours: u32,
    /// Argon2id parameters used for password hashing.
    pub argon2: Argon2Config,
}

/// Registration policy. `#[non_exhaustive]` because adding e.g. `OAuthOnly`
/// later should not be a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RegistrationPolicy {
    /// Anyone can register an account.
    Open,
    /// Registration requires an invite code.
    Invite,
    /// Registration is disabled. Administrators create accounts manually.
    Closed,
}

/// Which edit submissions land in the approval queue before going live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ApprovalScope {
    /// Edits go live immediately.
    None,
    /// Anonymous edits require approval; logged-in edits go live.
    Anonymous,
    /// Anonymous and new (recently-registered) accounts require approval.
    NewUsers,
    /// Every edit requires approval. Useful for high-security deploys.
    All,
}

/// Moderation policy — currently scopes the edit approval queue (#40).
///
/// Carved out of [`AuthConfig`] because the approval-queue is a
/// moderation concern; auth covers "who can sign in" and the moderation
/// section covers "which classes of edit need a human review before
/// going live". The two interact (anonymous edits typically map to the
/// "anonymous" approval scope) but they configure different teams in
/// practice.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModerationConfig {
    /// Edit approval queue policy.
    #[serde(default)]
    pub approval: ApprovalConfig,
}

/// Approval queue knobs surfaced under `[moderation.approval]` in the
/// TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalConfig {
    /// Which class of edits the moderator queue gates. The legacy
    /// [`AuthConfig::approval_required_for`] field is consulted as a
    /// fallback so existing config files keep working — the
    /// `moderation.approval.require_approval_for` knob is the new home
    /// for the same policy.
    #[serde(default = "default_require_approval_for")]
    pub require_approval_for: ApprovalRequirement,
    /// Window in days during which a freshly-registered account counts
    /// as a "new user" for the
    /// [`ApprovalRequirement::AnonAndNewUsers`] scope. Defaults to 7.
    #[serde(default = "default_new_user_threshold_days")]
    pub new_user_threshold_days: u32,
}

impl Default for ApprovalConfig {
    fn default() -> Self {
        Self {
            require_approval_for: default_require_approval_for(),
            new_user_threshold_days: default_new_user_threshold_days(),
        }
    }
}

fn default_require_approval_for() -> ApprovalRequirement {
    ApprovalRequirement::None
}

fn default_new_user_threshold_days() -> u32 {
    7
}

/// Approval-queue scope expressed as wire-form strings (mirrors what
/// operators write in `thewiki.toml`).
///
/// This is the public, version-stable shape used by
/// [`ApprovalConfig::require_approval_for`]. The legacy
/// [`ApprovalScope`] enum stays as the internal currency the page
/// handlers consume — [`ApprovalRequirement`] is converted to it via
/// [`ApprovalRequirement::into_scope`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApprovalRequirement {
    /// Every edit goes live immediately.
    None,
    /// Anonymous edits require approval; signed-in edits go live.
    Anon,
    /// Anonymous edits + edits from accounts younger than
    /// [`ApprovalConfig::new_user_threshold_days`] require approval.
    AnonAndNewUsers,
    /// Every edit requires approval.
    All,
}

impl ApprovalRequirement {
    /// Map the operator-facing string form to the internal
    /// [`ApprovalScope`] consumed by the page handlers.
    #[must_use]
    pub const fn into_scope(self) -> ApprovalScope {
        match self {
            Self::None => ApprovalScope::None,
            Self::Anon => ApprovalScope::Anonymous,
            Self::AnonAndNewUsers => ApprovalScope::NewUsers,
            Self::All => ApprovalScope::All,
        }
    }
}

/// Snapshot of the effective approval policy after merging the legacy
/// [`AuthConfig::approval_required_for`] field and the new
/// [`ModerationConfig::approval`] section.
///
/// The merge rule is: if the operator set the modern `[moderation.approval]`
/// section to anything other than the default, use it; otherwise fall back
/// to the legacy [`AuthConfig::approval_required_for`]. This keeps existing
/// configs (and the matching integration tests) working unchanged while
/// letting new deploys configure the queue through the documented path.
#[derive(Debug, Clone, Copy)]
pub struct EffectiveApprovalPolicy {
    /// Which class of edits should land in the queue.
    pub scope: ApprovalScope,
    /// Days after registration during which an account counts as "new"
    /// for [`ApprovalScope::NewUsers`].
    pub new_user_threshold_days: u32,
}

impl Config {
    /// Compute the effective approval policy a request handler should
    /// consult. See [`EffectiveApprovalPolicy`] for the merge rule.
    #[must_use]
    pub fn effective_approval_policy(&self) -> EffectiveApprovalPolicy {
        let modern_scope = self.moderation.approval.require_approval_for.into_scope();
        let scope = if matches!(modern_scope, ApprovalScope::None) {
            // Modern config left as the default — defer to the legacy field
            // so existing configs keep working. Once the legacy field is
            // retired this branch collapses to `modern_scope`.
            self.auth.approval_required_for
        } else {
            modern_scope
        };
        EffectiveApprovalPolicy {
            scope,
            new_user_threshold_days: self.moderation.approval.new_user_threshold_days,
        }
    }
}

/// Argon2id tuning. Defaults target a sensible production posture
/// (64 MiB / 3 iterations / 1 lane).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Argon2Config {
    /// Memory cost in KiB. OWASP recommends ≥ 19 MiB (19456 KiB).
    pub memory_kib: u32,
    /// Number of iterations (time cost). OWASP recommends ≥ 2.
    pub iterations: u32,
    /// Parallelism (lanes). OWASP recommends ≥ 1.
    pub parallelism: u32,
}

/// Rate limiting configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitConfig {
    /// Global switch. Keep enabled in production; tests and trusted private
    /// deployments can disable it explicitly.
    pub enabled: bool,
    /// Bucket used for safe/read methods (`GET`, `HEAD`, `OPTIONS`) by
    /// anonymous requests. Authenticated users use [`Self::authenticated_read`]
    /// when set, falling back to this bucket otherwise.
    pub read: RateLimitBucketConfig,
    /// Bucket used for mutating methods (`POST`, `PUT`, `PATCH`, `DELETE`, ...)
    /// by anonymous requests. Authenticated users use
    /// [`Self::authenticated_write`] when set, falling back to this bucket
    /// otherwise.
    pub write: RateLimitBucketConfig,
    /// Read bucket override for authenticated users. Typically higher than
    /// [`Self::read`] — operators trust signed-in users more. When `None`,
    /// authenticated reads share the anonymous bucket.
    #[serde(default)]
    pub authenticated_read: Option<RateLimitBucketConfig>,
    /// Write bucket override for authenticated users. Typically higher than
    /// [`Self::write`]. When `None`, authenticated writes share the anonymous
    /// bucket.
    #[serde(default)]
    pub authenticated_write: Option<RateLimitBucketConfig>,
    /// Optional proxy header used to derive the client IP. Only honored when
    /// the socket peer is in `trusted_proxies`.
    #[serde(default)]
    pub client_ip_header: Option<ClientIpHeader>,
    /// Proxy IPs that are allowed to supply `client_ip_header`.
    #[serde(default)]
    pub trusted_proxies: Vec<IpAddr>,
    /// Storage backend used for bucket state.
    pub backend: RateLimitBackendConfig,
}

/// Token-bucket shape for one request class.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitBucketConfig {
    /// Maximum burst size.
    pub capacity: u32,
    /// Number of tokens restored every `refill_interval_secs`.
    pub refill_tokens: u32,
    /// Refill interval in seconds.
    pub refill_interval_secs: u64,
}

/// Reverse-proxy header used for rate-limit client IP extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ClientIpHeader {
    /// `X-Forwarded-For` chain. When the socket peer is trusted, the limiter
    /// scans the comma-separated list from right to left and selects the first
    /// IP that is not listed in `rate_limit.trusted_proxies`.
    XForwardedFor,
    /// Single IP in `X-Real-IP`.
    XRealIp,
}

/// Rate-limit bucket storage backend.
///
/// `InMemory` is the default and is suitable for single-replica deploys.
/// `Redis` requires building with `--features thewiki-api/redis`; the field
/// is always parseable so a multi-replica deploy can declare the intent in
/// its config file and only the binary's feature flags decide whether the
/// process can serve it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[non_exhaustive]
pub enum RateLimitBackendConfig {
    /// Per-process in-memory buckets. This is the default and is suitable for
    /// single-binary/single-replica deploys.
    InMemory,
    /// Shared bucket state in Redis. The URL is in `redis://` or `rediss://`
    /// form. Only honoured when the binary is built with the `redis` cargo
    /// feature.
    Redis {
        /// Redis connection URL (`redis://host:port[/db]`).
        url: String,
    },
}

/// Full-text search (Tantivy) configuration (#26).
///
/// `index_path` is where the Tantivy segments live on disk. The default
/// (`./data/search/`) keeps the index next to the SQLite database for
/// single-binary deploys. Operators running on a separate disk (or a
/// network mount) typically override this.
///
/// `commit_interval_ms` / `batch_size` tune the async indexer worker —
/// lowering either improves freshness at the cost of write amplification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    /// Where the Tantivy index lives on disk. The directory is created on
    /// first boot.
    pub index_path: PathBuf,
    /// Commit cadence, in milliseconds. The worker commits every
    /// `commit_interval_ms` or every `batch_size` jobs, whichever comes
    /// first. Default: 200 ms.
    pub commit_interval_ms: u64,
    /// Per-commit job-count threshold. Default: 100.
    pub batch_size: u32,
    /// Multiplier applied to the `title` field during BM25 ranking. Values
    /// above 1.0 promote title matches; `0.0` disables the boost (every
    /// field weighted equally). Default: `2.0`.
    #[serde(default = "default_title_boost")]
    pub title_boost: f32,
}

/// `serde` default for [`SearchConfig::title_boost`].
#[must_use]
fn default_title_boost() -> f32 {
    2.0
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            index_path: PathBuf::from("data/search"),
            commit_interval_ms: 200,
            batch_size: 100,
            title_boost: default_title_boost(),
        }
    }
}

/// Audit-log storage policy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditLogConfig {
    /// Master switch. When `false`, the API still serves the read endpoints
    /// (so operators can inspect historical rows) but the background pruner
    /// is skipped and the `audit-log-prune` xtask will refuse to run. Default
    /// is `true`.
    #[serde(default = "default_audit_log_enabled")]
    pub enabled: bool,
    /// Number of days to retain audit rows. Defaults to 365.
    pub retention_days: u32,
}

/// `serde` default for [`AuditLogConfig::enabled`].
#[must_use]
fn default_audit_log_enabled() -> bool {
    true
}

/// Observability configuration consumed by [`crate::telemetry`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    pub log_format: LogFormat,
    /// `tracing_subscriber` env-filter directive (e.g. `"info,thewiki=debug"`).
    pub log_filter: String,
}

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum LogFormat {
    /// Structured JSON, one event per line. Default — friendly to log shippers.
    Json,
    /// Human-readable pretty output. Use during local development.
    Pretty,
}

/// Errors surfaced by [`Config::load`] and [`Config::validate`].
///
/// `#[non_exhaustive]` so we can grow the enum (e.g. add `Io`) without breaking
/// callers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// An explicit config file path was supplied but the file does not exist.
    #[error("config file not found: {0}")]
    NotFound(PathBuf),

    /// `figment` failed to parse or merge the layered providers.
    ///
    /// Boxed because `figment::Error` is wide (~200 B) and pushes the whole
    /// enum into clippy's `result_large_err` warning bucket.
    #[error("failed to parse configuration: {0}")]
    Parse(#[from] Box<figment::Error>),

    /// Cross-field validation failed (e.g. empty `database.url`, unparseable
    /// bind address, or Argon2 parameters below the OWASP floor).
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

// Minimum Argon2id parameters, per OWASP password storage cheat sheet (2024).
// Going below these is a misconfiguration that we reject up front.
const ARGON2_MIN_MEMORY_KIB: u32 = 19_456;
const ARGON2_MIN_ITERATIONS: u32 = 2;
const ARGON2_MIN_PARALLELISM: u32 = 1;

impl Config {
    /// Built-in defaults.
    ///
    /// Tuned for an out-of-the-box small deploy: SQLite under `data/`, blobs in
    /// the DB, registration closed, JSON logs.
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            server: ServerConfig {
                bind: "0.0.0.0:8080".to_string(),
                request_timeout: Some("30s".to_string()),
                worker_threads: None,
                serve_frontend: default_serve_frontend(),
            },
            database: DatabaseConfig {
                driver: DatabaseDriver::Sqlite,
                url: "sqlite://data/thewiki.db".to_string(),
                max_connections: 16,
                acquire_timeout_secs: 10,
            },
            storage: StorageConfig {
                backend: StorageBackend::Db,
                media: MediaConfig::default(),
            },
            auth: AuthConfig {
                anonymous_edits: false,
                registration: RegistrationPolicy::Closed,
                approval_required_for: ApprovalScope::None,
                session_ttl_hours: 24,
                argon2: Argon2Config {
                    memory_kib: 65_536,
                    iterations: 3,
                    parallelism: 1,
                },
            },
            search: SearchConfig::default(),
            // Opinionated defaults the operator will almost certainly tune.
            // Anonymous quotas err on the side of *strict* — they're enough
            // for a normal browsing session and aggressive enough to make
            // bot scraping notice immediately. Authenticated quotas are 10×
            // higher; the signed-in user is identified and (typically)
            // bound by an account-level ToS, so the protection priority
            // shifts from "throttle abusers" to "absorb editing bursts".
            rate_limit: RateLimitConfig {
                enabled: true,
                // 60 reads/min per anonymous IP.
                read: RateLimitBucketConfig {
                    capacity: 60,
                    refill_tokens: 60,
                    refill_interval_secs: 60,
                },
                // 10 writes/min per anonymous IP.
                write: RateLimitBucketConfig {
                    capacity: 10,
                    refill_tokens: 10,
                    refill_interval_secs: 60,
                },
                // 600 reads/min per authenticated user.
                authenticated_read: Some(RateLimitBucketConfig {
                    capacity: 600,
                    refill_tokens: 600,
                    refill_interval_secs: 60,
                }),
                // 120 writes/min per authenticated user.
                authenticated_write: Some(RateLimitBucketConfig {
                    capacity: 120,
                    refill_tokens: 120,
                    refill_interval_secs: 60,
                }),
                client_ip_header: None,
                trusted_proxies: Vec::new(),
                backend: RateLimitBackendConfig::InMemory,
            },
            audit_log: AuditLogConfig {
                enabled: true,
                retention_days: 365,
            },
            telemetry: TelemetryConfig {
                log_format: LogFormat::Json,
                log_filter: "info,thewiki=debug".to_string(),
            },
            graphql: GraphQLConfig::default(),
            captcha: CaptchaConfig::default(),
            security: SecurityConfig::default(),
            render: RenderConfig::default(),
            moderation: ModerationConfig::default(),
        }
    }

    /// Load configuration, merging built-in defaults, optional TOML file, and
    /// the `THEWIKI_*` environment.
    ///
    /// # Errors
    ///
    /// - [`ConfigError::NotFound`] if `file_path` is `Some` but the file is
    ///   missing.
    /// - [`ConfigError::Parse`] for malformed TOML or env values that don't
    ///   deserialise into [`Config`].
    pub fn load(file_path: Option<&Path>) -> Result<Self, ConfigError> {
        let mut figment = Figment::from(Serialized::defaults(Self::defaults()));

        if let Some(path) = file_path {
            // Read the file ourselves rather than probing existence with
            // `exists()` followed by `Toml::file(path)` — the latter is a
            // TOCTOU race (the file can vanish between calls, especially
            // with mounted secrets in containers).
            let body = match std::fs::read_to_string(path) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(ConfigError::NotFound(path.to_path_buf()));
                }
                Err(e) => {
                    return Err(ConfigError::Invalid(format!(
                        "could not read config file {}: {e}",
                        path.display()
                    )));
                }
            };
            figment = figment.merge(Toml::string(&body));
        }

        // Double-underscore separator: `THEWIKI_SERVER__BIND` -> `server.bind`.
        // A single underscore would collide with snake_case field names like
        // `acquire_timeout_secs`.
        figment = figment.merge(Env::prefixed("THEWIKI_").split("__"));

        let cfg: Self = figment.extract().map_err(Box::new)?;
        Ok(cfg)
    }

    /// Cross-field validation. Run after [`Config::load`] before standing up
    /// the runtime.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Invalid`] when any of the structural invariants
    /// fail (empty DB URL, unparseable bind address, Argon2 params below the
    /// OWASP floor, ...).
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.database.url.trim().is_empty() {
            return Err(ConfigError::Invalid("database.url is empty".to_string()));
        }

        if self.server.bind.parse::<SocketAddr>().is_err() {
            return Err(ConfigError::Invalid(format!(
                "server.bind is not a valid socket address: {:?}",
                self.server.bind
            )));
        }

        let a = &self.auth.argon2;
        if a.memory_kib < ARGON2_MIN_MEMORY_KIB {
            return Err(ConfigError::Invalid(format!(
                "auth.argon2.memory_kib = {} is below the OWASP floor of {} KiB",
                a.memory_kib, ARGON2_MIN_MEMORY_KIB
            )));
        }
        if a.iterations < ARGON2_MIN_ITERATIONS {
            return Err(ConfigError::Invalid(format!(
                "auth.argon2.iterations = {} is below the OWASP floor of {}",
                a.iterations, ARGON2_MIN_ITERATIONS
            )));
        }
        if a.parallelism < ARGON2_MIN_PARALLELISM {
            return Err(ConfigError::Invalid(format!(
                "auth.argon2.parallelism = {} is below the OWASP floor of {}",
                a.parallelism, ARGON2_MIN_PARALLELISM
            )));
        }

        // Soft-warn combinations that aren't strictly invalid but tend to bite
        // operators. Tracing is initialised by the binary before validate is
        // called, so these are visible in startup logs.
        if self.auth.anonymous_edits && self.auth.registration == RegistrationPolicy::Closed {
            tracing::warn!(
                "auth.anonymous_edits = true with auth.registration = closed: anonymous \
                 visitors can edit but cannot create accounts to take ownership of edits"
            );
        }

        if let StorageBackend::S3 { bucket, region, .. } = &self.storage.backend
            && (bucket.trim().is_empty() || region.trim().is_empty())
        {
            return Err(ConfigError::Invalid(
                "storage.backend = s3 requires both `bucket` and `region` to be non-empty"
                    .to_string(),
            ));
        }
        if self.storage.media.max_upload_bytes == 0 {
            return Err(ConfigError::Invalid(
                "storage.media.max_upload_bytes must be > 0".to_string(),
            ));
        }
        if self.storage.media.allowed_content_types.is_empty() {
            return Err(ConfigError::Invalid(
                "storage.media.allowed_content_types must list at least one MIME type".to_string(),
            ));
        }

        validate_rate_limit_bucket("rate_limit.read", &self.rate_limit.read)?;
        validate_rate_limit_bucket("rate_limit.write", &self.rate_limit.write)?;
        if let Some(b) = &self.rate_limit.authenticated_read {
            validate_rate_limit_bucket("rate_limit.authenticated_read", b)?;
        }
        if let Some(b) = &self.rate_limit.authenticated_write {
            validate_rate_limit_bucket("rate_limit.authenticated_write", b)?;
        }
        if self.rate_limit.client_ip_header.is_some() && self.rate_limit.trusted_proxies.is_empty()
        {
            return Err(ConfigError::Invalid(
                "rate_limit.client_ip_header requires at least one trusted proxy".to_string(),
            ));
        }
        if let RateLimitBackendConfig::Redis { url } = &self.rate_limit.backend
            && url.trim().is_empty()
        {
            return Err(ConfigError::Invalid(
                "rate_limit.backend = redis requires a non-empty `url`".to_string(),
            ));
        }
        if self.audit_log.retention_days == 0 {
            return Err(ConfigError::Invalid(
                "audit_log.retention_days must be > 0".to_string(),
            ));
        }

        if self.search.index_path.as_os_str().is_empty() {
            return Err(ConfigError::Invalid(
                "search.index_path must be non-empty".to_string(),
            ));
        }
        if self.search.commit_interval_ms == 0 {
            return Err(ConfigError::Invalid(
                "search.commit_interval_ms must be > 0".to_string(),
            ));
        }
        if self.search.batch_size == 0 {
            return Err(ConfigError::Invalid(
                "search.batch_size must be > 0".to_string(),
            ));
        }
        if !self.search.title_boost.is_finite() || self.search.title_boost < 0.0 {
            return Err(ConfigError::Invalid(
                "search.title_boost must be a finite non-negative number".to_string(),
            ));
        }

        if self.graphql.max_query_depth == 0 {
            return Err(ConfigError::Invalid(
                "graphql.max_query_depth must be > 0".to_string(),
            ));
        }
        if self.graphql.max_query_complexity == 0 {
            return Err(ConfigError::Invalid(
                "graphql.max_query_complexity must be > 0".to_string(),
            ));
        }

        // CAPTCHA: when an upstream provider is selected, both keys must be
        // set. A half-configured provider would fail at the first request
        // anyway; we want it loud at startup instead.
        if self.captcha.provider == CaptchaProviderKind::Hcaptcha {
            if self.captcha.site_key.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "captcha.site_key must be non-empty when captcha.provider = \"hcaptcha\""
                        .to_string(),
                ));
            }
            if self.captcha.secret_key.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "captcha.secret_key must be non-empty when captcha.provider = \"hcaptcha\""
                        .to_string(),
                ));
            }
        }

        // Security/blocklist (#42): make sure every operator-supplied CIDR
        // parses as an `ipnet::IpNet`. We do this at config load so a typo
        // in `thewiki.toml` surfaces as a clean startup error rather than
        // an opaque runtime warning when the middleware tries to use it.
        for cidr in &self.security.trusted_proxies {
            if cidr.parse::<ipnet::IpNet>().is_err() {
                return Err(ConfigError::Invalid(format!(
                    "security.trusted_proxies: {cidr:?} is not a valid CIDR"
                )));
            }
        }

        if self.render.template.max_recursion_depth == 0 {
            return Err(ConfigError::Invalid(
                "render.template.max_recursion_depth must be > 0".to_string(),
            ));
        }

        Ok(())
    }
}

fn validate_rate_limit_bucket(
    name: &str,
    bucket: &RateLimitBucketConfig,
) -> Result<(), ConfigError> {
    if bucket.capacity == 0 {
        return Err(ConfigError::Invalid(format!("{name}.capacity must be > 0")));
    }
    if bucket.refill_tokens == 0 {
        return Err(ConfigError::Invalid(format!(
            "{name}.refill_tokens must be > 0"
        )));
    }
    if bucket.refill_interval_secs == 0 {
        return Err(ConfigError::Invalid(format!(
            "{name}.refill_interval_secs must be > 0"
        )));
    }
    Ok(())
}

impl Default for Config {
    fn default() -> Self {
        Self::defaults()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let cfg = Config::defaults();
        cfg.validate().expect("built-in defaults must validate");
    }

    #[test]
    fn validate_rejects_empty_database_url() {
        let mut cfg = Config::defaults();
        cfg.database.url = String::new();
        let err = cfg.validate().expect_err("empty url must be rejected");
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn validate_rejects_unparseable_bind() {
        let mut cfg = Config::defaults();
        cfg.server.bind = "not-an-addr".to_string();
        let err = cfg.validate().expect_err("bad bind must be rejected");
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn validate_rejects_weak_argon2() {
        let mut cfg = Config::defaults();
        cfg.auth.argon2.memory_kib = 1024;
        let err = cfg.validate().expect_err("weak argon2 must be rejected");
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn validate_rejects_hcaptcha_with_missing_keys() {
        let mut cfg = Config::defaults();
        cfg.captcha.provider = CaptchaProviderKind::Hcaptcha;
        let err = cfg
            .validate()
            .expect_err("hcaptcha without site_key must be rejected");
        let msg = match err {
            ConfigError::Invalid(m) => m,
            other => panic!("expected Invalid, got {other:?}"),
        };
        assert!(msg.contains("site_key"), "{msg}");

        cfg.captcha.site_key = "public-site".to_string();
        let err = cfg
            .validate()
            .expect_err("hcaptcha without secret_key must be rejected");
        let msg = match err {
            ConfigError::Invalid(m) => m,
            other => panic!("expected Invalid, got {other:?}"),
        };
        assert!(msg.contains("secret_key"), "{msg}");

        cfg.captcha.secret_key = "server-side".to_string();
        cfg.validate().expect("fully configured hcaptcha validates");
    }

    #[test]
    fn captcha_default_is_noop_with_registration_gate_enabled() {
        let cfg = Config::defaults();
        assert_eq!(cfg.captcha.provider, CaptchaProviderKind::Noop);
        assert!(cfg.captcha.apply_to_registration);
        assert!(!cfg.captcha.apply_to_anonymous_edits);
    }
}
