//! Application state passed to Axum handlers.
//!
//! [`AppState`] carries a single cloneable storage handle plus runtime tuning
//! the route handlers need (e.g. the default page-list size). It is generic
//! over the storage facade so the API crate stays decoupled from the
//! concrete SQLite/Postgres/libsql implementation.
//!
//! ## Why a trait instead of `Arc<dyn …>`?
//!
//! The repository traits in [`thewiki_storage::repo`] use native
//! `async fn` in trait, which is not `dyn`-compatible on stable today (see the
//! design note in `crates/storage/src/repo.rs`). The intentional path is
//! monomorphisation via generics: [`AppStorage`] abstracts over the *storage
//! handle* (a cheap-to-clone facade like
//! [`thewiki_storage::sqlite::SqliteStorage`]), and each handler reaches for
//! the per-aggregate repository via the accessor methods.

use std::sync::Arc;

use axum::extract::FromRef;
use thewiki_core::{CaptchaProvider, NoopCaptcha};
use thewiki_search::{IndexerHandle, Searcher};
use thewiki_storage::StorageError;
use thewiki_storage::repo::{
    AuditLogRepository, CategoryRepository, IpBlocklistRepository, MediaBlobRepository,
    MediaRepository, MediaVariantRepository, NamespaceRepository, NewAuditLogEntry,
    NotificationRepository, PageAuditMutation, PageLinkRepository, PageRepository,
    PendingRevisionRepository, RecentChangesRepository, RevisionRepository, TagRepository,
    UrlBlocklistRepository, UserRepository, WatchRepository,
};

use crate::auth::AuthState;
use crate::blocklist::BlocklistState;
use crate::config::{AuthConfig, CaptchaConfig, EffectiveApprovalPolicy, ModerationConfig};
use crate::media::MediaBackend;

/// A cloneable storage facade that hands out per-aggregate repositories.
///
/// Implemented by the concrete storage facades (e.g.
/// [`thewiki_storage::sqlite::SqliteStorage`]) so the API layer can stay
/// generic. The lifetime-bound associated types mirror the pattern the
/// SQLite repositories use today — repositories borrow the pool for the
/// duration of the call rather than each owning their own `Arc<Pool>`.
pub trait AppStorage: Clone + Send + Sync + 'static {
    /// Page repository borrowed from this handle.
    type Pages<'a>: PageRepository + 'a
    where
        Self: 'a;
    /// Revision repository borrowed from this handle.
    type Revisions<'a>: RevisionRepository + 'a
    where
        Self: 'a;
    /// Namespace repository borrowed from this handle.
    type Namespaces<'a>: NamespaceRepository + 'a
    where
        Self: 'a;
    /// Recent-changes repository borrowed from this handle.
    type RecentChanges<'a>: RecentChangesRepository + 'a
    where
        Self: 'a;
    /// Audit-log repository borrowed from this handle.
    type AuditLog<'a>: AuditLogRepository + 'a
    where
        Self: 'a;
    /// User repository borrowed from this handle.
    type Users<'a>: UserRepository + 'a
    where
        Self: 'a;
    /// Page-link (wikilink graph) repository borrowed from this handle.
    type PageLinks<'a>: PageLinkRepository + 'a
    where
        Self: 'a;
    /// Media metadata repository borrowed from this handle (#32).
    type Media<'a>: MediaRepository + 'a
    where
        Self: 'a;
    /// Media blob repository borrowed from this handle (#32). Only used by
    /// the in-DB blob backend; the S3 backend uses `object_store` directly.
    type MediaBlobs<'a>: MediaBlobRepository + 'a
    where
        Self: 'a;
    /// Media variant (thumbnail) repository borrowed from this handle (#33).
    type MediaVariants<'a>: MediaVariantRepository + 'a
    where
        Self: 'a;
    /// Category repository borrowed from this handle (#29).
    type Categories<'a>: CategoryRepository + 'a
    where
        Self: 'a;
    /// Tag repository borrowed from this handle (#29).
    type Tags<'a>: TagRepository + 'a
    where
        Self: 'a;
    /// IP blocklist repository borrowed from this handle (#42).
    type IpBlocklist<'a>: IpBlocklistRepository + 'a
    where
        Self: 'a;
    /// URL blocklist repository borrowed from this handle (#42).
    type UrlBlocklist<'a>: UrlBlocklistRepository + 'a
    where
        Self: 'a;
    /// Watch repository borrowed from this handle (#46).
    type Watches<'a>: WatchRepository + 'a
    where
        Self: 'a;
    /// Pending-revision (edit approval queue) repository borrowed from this
    /// handle (#40).
    type PendingRevisions<'a>: PendingRevisionRepository + 'a
    where
        Self: 'a;
    /// Notification (in-app inbox) repository borrowed from this handle (#40).
    type Notifications<'a>: NotificationRepository + 'a
    where
        Self: 'a;

    /// Borrow a [`PageRepository`].
    fn pages(&self) -> Self::Pages<'_>;
    /// Borrow a [`RevisionRepository`].
    fn revisions(&self) -> Self::Revisions<'_>;
    /// Borrow a [`NamespaceRepository`].
    fn namespaces(&self) -> Self::Namespaces<'_>;
    /// Borrow a [`RecentChangesRepository`].
    fn recent_changes(&self) -> Self::RecentChanges<'_>;
    /// Borrow an [`AuditLogRepository`].
    fn audit_log(&self) -> Self::AuditLog<'_>;
    /// Borrow a [`UserRepository`].
    fn users(&self) -> Self::Users<'_>;
    /// Borrow a [`PageLinkRepository`] (powers backlinks API, #30).
    fn page_links(&self) -> Self::PageLinks<'_>;
    /// Borrow a [`MediaRepository`] (powers media uploads, #32).
    fn media(&self) -> Self::Media<'_>;
    /// Borrow a [`MediaBlobRepository`] (powers the in-DB media backend, #32).
    fn media_blobs(&self) -> Self::MediaBlobs<'_>;
    /// Borrow a [`MediaVariantRepository`] (powers thumbnails, #33).
    fn media_variants(&self) -> Self::MediaVariants<'_>;
    /// Borrow a [`CategoryRepository`] (#29).
    fn categories(&self) -> Self::Categories<'_>;
    /// Borrow a [`TagRepository`] (#29).
    fn tags(&self) -> Self::Tags<'_>;
    /// Borrow an [`IpBlocklistRepository`] (#42).
    fn ip_blocklist(&self) -> Self::IpBlocklist<'_>;
    /// Borrow a [`UrlBlocklistRepository`] (#42).
    fn url_blocklist(&self) -> Self::UrlBlocklist<'_>;
    /// Borrow a [`WatchRepository`] (#46).
    fn watches(&self) -> Self::Watches<'_>;
    /// Borrow a [`PendingRevisionRepository`] (#40).
    fn pending_revisions(&self) -> Self::PendingRevisions<'_>;
    /// Borrow a [`NotificationRepository`] (#40).
    fn notifications(&self) -> Self::Notifications<'_>;

    /// Commit a page mutation and its required audit row atomically.
    fn commit_page_audit(
        &self,
        mutation: PageAuditMutation,
        audit: NewAuditLogEntry,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;
}

impl AppStorage for thewiki_storage::sqlite::SqliteStorage {
    type Pages<'a> = thewiki_storage::sqlite::SqlitePageRepository<'a>;
    type Revisions<'a> = thewiki_storage::sqlite::SqliteRevisionRepository<'a>;
    type Namespaces<'a> = thewiki_storage::sqlite::SqliteNamespaceRepository<'a>;
    type RecentChanges<'a> = thewiki_storage::sqlite::SqliteRecentChangesRepository<'a>;
    type AuditLog<'a> = thewiki_storage::sqlite::SqliteAuditLogRepository<'a>;
    type Users<'a> = thewiki_storage::sqlite::SqliteUserRepository<'a>;
    type PageLinks<'a> = thewiki_storage::sqlite::SqlitePageLinkRepository<'a>;
    type Media<'a> = thewiki_storage::sqlite::SqliteMediaRepository<'a>;
    type MediaBlobs<'a> = thewiki_storage::sqlite::SqliteMediaBlobRepository<'a>;
    type MediaVariants<'a> = thewiki_storage::sqlite::SqliteMediaVariantRepository<'a>;
    type Categories<'a> = thewiki_storage::sqlite::SqliteCategoryRepository<'a>;
    type Tags<'a> = thewiki_storage::sqlite::SqliteTagRepository<'a>;
    type IpBlocklist<'a> = thewiki_storage::sqlite::SqliteIpBlocklistRepository<'a>;
    type UrlBlocklist<'a> = thewiki_storage::sqlite::SqliteUrlBlocklistRepository<'a>;
    type Watches<'a> = thewiki_storage::sqlite::SqliteWatchRepository<'a>;
    type PendingRevisions<'a> = thewiki_storage::sqlite::SqlitePendingRevisionRepository<'a>;
    type Notifications<'a> = thewiki_storage::sqlite::SqliteNotificationRepository<'a>;

    fn pages(&self) -> Self::Pages<'_> {
        Self::pages(self)
    }
    fn revisions(&self) -> Self::Revisions<'_> {
        Self::revisions(self)
    }
    fn namespaces(&self) -> Self::Namespaces<'_> {
        Self::namespaces(self)
    }
    fn recent_changes(&self) -> Self::RecentChanges<'_> {
        Self::recent_changes(self)
    }
    fn audit_log(&self) -> Self::AuditLog<'_> {
        Self::audit_log(self)
    }
    fn users(&self) -> Self::Users<'_> {
        Self::users(self)
    }
    fn page_links(&self) -> Self::PageLinks<'_> {
        Self::page_links(self)
    }
    fn media(&self) -> Self::Media<'_> {
        Self::media(self)
    }
    fn media_blobs(&self) -> Self::MediaBlobs<'_> {
        Self::media_blobs(self)
    }
    fn media_variants(&self) -> Self::MediaVariants<'_> {
        Self::media_variants(self)
    }
    fn categories(&self) -> Self::Categories<'_> {
        Self::categories(self)
    }
    fn tags(&self) -> Self::Tags<'_> {
        Self::tags(self)
    }
    fn ip_blocklist(&self) -> Self::IpBlocklist<'_> {
        Self::ip_blocklist(self)
    }
    fn url_blocklist(&self) -> Self::UrlBlocklist<'_> {
        Self::url_blocklist(self)
    }
    fn watches(&self) -> Self::Watches<'_> {
        Self::watches(self)
    }
    fn pending_revisions(&self) -> Self::PendingRevisions<'_> {
        Self::pending_revisions(self)
    }
    fn notifications(&self) -> Self::Notifications<'_> {
        Self::notifications(self)
    }

    fn commit_page_audit(
        &self,
        mutation: PageAuditMutation,
        audit: NewAuditLogEntry,
    ) -> impl Future<Output = Result<(), StorageError>> + Send {
        Self::commit_page_audit(self, mutation, audit)
    }
}

/// Routing-time tuning knobs.
///
/// Carved out of [`crate::config::Config`] so the route layer doesn't reach
/// back into the binary's full config tree. Cheap to clone.
#[derive(Debug, Clone, Copy)]
pub struct RouteConfig {
    /// Page size used when a list endpoint receives no `limit` query param.
    /// Hard upper bound is enforced by the storage layer
    /// ([`thewiki_storage::repo::MAX_PAGE_SIZE`]).
    pub default_page_size: u32,
}

impl Default for RouteConfig {
    fn default() -> Self {
        Self {
            default_page_size: thewiki_storage::repo::DEFAULT_PAGE_SIZE,
        }
    }
}

/// Application state, cloned cheaply into every request.
///
/// `S` is the storage facade (typically
/// [`thewiki_storage::sqlite::SqliteStorage`]); see [`AppStorage`].
///
/// The `auth_config` snapshot is the wired runtime view of
/// [`crate::config::AuthConfig`] — handlers consult it to decide whether to
/// require a session, whether to gate edits into the (M2) approval queue, and
/// what registration policy to advertise via `GET /api/v1/auth/policy`.
///
/// `auth_state` is optional because some integration tests boot just the
/// pages router without standing up the auth stack. When `None`, the
/// configurable-auth extractors fall back to "no session was supplied" — i.e.
/// every caller is treated as anonymous. Production (`build_full`) always
/// supplies one.
pub struct AppState<S: AppStorage> {
    /// Shared storage handle.
    pub storage: Arc<S>,
    /// Per-route configuration knobs.
    pub route_config: RouteConfig,
    /// Snapshot of `Config::auth` — the configurable-auth wiring point (#14).
    pub auth_config: AuthConfig,
    /// Snapshot of `Config::moderation` — the approval queue policy (#40).
    pub moderation_config: ModerationConfig,
    /// Auth state shared with the auth router (cookies, hasher, session TTL).
    /// `None` in test fixtures that don't exercise the auth stack.
    pub auth_state: Option<AuthState>,
    /// Handle to the async Tantivy indexer (#26). Disabled in tests and in
    /// `build_with_state` callers that don't stand up the worker; the
    /// page-CRUD handlers still call the handle but the no-op variant
    /// drops every job.
    pub search: IndexerHandle,
    /// Read-side handle to the same Tantivy index the indexer writes to
    /// (#27). Cloneable; disabled in tests / callers that don't stand up
    /// the index — queries against a disabled handle return an empty
    /// result set rather than an error.
    pub searcher: Searcher,
    /// Title-field boost passed through to the Tantivy `QueryParser`.
    /// Pulled from [`crate::config::SearchConfig::title_boost`].
    pub search_title_boost: f32,
    /// Multiplier applied to the BM25 score of pages that live in a
    /// discussion / talk namespace (#43). Pulled from
    /// [`crate::config::SearchConfig::talk_boost`] (default `0.5`).
    pub search_talk_boost: f32,
    /// Tuning for the media upload endpoint (size cap, type allowlist).
    /// Pulled from [`crate::config::StorageConfig::media`].
    pub media_config: crate::config::MediaConfig,
    /// Renderer tuning — currently the template transclusion depth cap
    /// (#45). Pulled from [`crate::config::RenderConfig`].
    pub render_config: crate::config::RenderConfig,
    /// Blob backend for the media endpoints (#32). `None` in tests / app
    /// roots that don't wire media routes; otherwise an `Arc<dyn …>`
    /// because [`MediaBackend`] is dyn-compatible.
    pub media_backend: Option<Arc<dyn MediaBackend>>,
    /// CAPTCHA provider (#41). Defaults to `Arc<NoopCaptcha>` so test
    /// fixtures don't have to know about the captcha wiring. Production
    /// wires the operator-configured provider via [`Self::with_captcha`].
    pub captcha: Arc<dyn CaptchaProvider>,
    /// Snapshot of `Config::captcha` so handlers can branch on the
    /// `apply_to_*` flags without reading the wider `AppState`. Cloned
    /// cheaply (the type is small `String` + `bool` fields).
    pub captcha_config: CaptchaConfig,
    /// In-memory IP / URL blocklist snapshot shared with the middleware
    /// and the admin endpoints (#42). `None` in test fixtures that don't
    /// wire the layer; otherwise refreshed on boot and on every admin
    /// mutation.
    pub blocklist: Option<BlocklistState>,
}

impl<S: AppStorage> AppState<S> {
    /// Build a new [`AppState`] from a storage handle and the configured auth
    /// snapshot. The default route config is applied; the auth state is
    /// initialised to `None` (suitable for tests that don't stand up the auth
    /// stack — production wiring uses [`Self::with_auth_state`]).
    #[must_use]
    pub fn new(storage: S, auth_config: AuthConfig) -> Self {
        Self {
            storage: Arc::new(storage),
            route_config: RouteConfig::default(),
            auth_config,
            moderation_config: ModerationConfig::default(),
            auth_state: None,
            search: IndexerHandle::disabled(),
            searcher: Searcher::disabled(),
            search_title_boost: 2.0,
            search_talk_boost: 0.5,
            media_config: crate::config::MediaConfig::default(),
            media_backend: None,
            captcha: Arc::new(NoopCaptcha),
            captcha_config: CaptchaConfig::default(),
            blocklist: None,
            render_config: crate::config::RenderConfig::default(),
        }
    }

    /// Override the renderer configuration (template depth cap, etc.). The
    /// binary wires this from `[render]` in the operator's TOML.
    #[must_use]
    pub fn with_render_config(mut self, render: crate::config::RenderConfig) -> Self {
        self.render_config = render;
        self
    }

    /// Replace the [`ModerationConfig`] snapshot. Production wiring fills
    /// this from `Config::moderation` so the approval queue handlers see
    /// the operator's policy.
    #[must_use]
    pub fn with_moderation_config(mut self, moderation: ModerationConfig) -> Self {
        self.moderation_config = moderation;
        self
    }

    /// Compute the effective approval policy (merging the legacy
    /// `auth.approval_required_for` field with the modern
    /// `[moderation.approval]` section, see
    /// [`crate::config::Config::effective_approval_policy`]).
    #[must_use]
    pub fn effective_approval_policy(&self) -> EffectiveApprovalPolicy {
        let modern_scope = self
            .moderation_config
            .approval
            .require_approval_for
            .into_scope();
        let scope = if matches!(modern_scope, crate::config::ApprovalScope::None) {
            self.auth_config.approval_required_for
        } else {
            modern_scope
        };
        EffectiveApprovalPolicy {
            scope,
            new_user_threshold_days: self.moderation_config.approval.new_user_threshold_days,
        }
    }


    /// Replace the [`IndexerHandle`]. Production code calls this with a
    /// handle minted by [`thewiki_search::Indexer::spawn`]; tests typically
    /// leave the default disabled handle in place.
    #[must_use]
    pub fn with_search(mut self, search: IndexerHandle) -> Self {
        self.search = search;
        self
    }

    /// Replace the [`Searcher`] used by the read-side search endpoint.
    ///
    /// Production code constructs this with the same `Arc<SearchIndex>` the
    /// indexer worker writes against, so committed updates are visible
    /// through the reader as Tantivy's commit-reload window expires.
    #[must_use]
    pub fn with_searcher(mut self, searcher: Searcher) -> Self {
        self.searcher = searcher;
        self
    }

    /// Override the title-field boost applied to BM25 ranking. Wired from
    /// [`crate::config::SearchConfig::title_boost`].
    #[must_use]
    pub fn with_search_title_boost(mut self, boost: f32) -> Self {
        self.search_title_boost = boost;
        self
    }

    /// Override the talk-namespace score multiplier (#43). Wired from
    /// [`crate::config::SearchConfig::talk_boost`].
    #[must_use]
    pub fn with_search_talk_boost(mut self, boost: f32) -> Self {
        self.search_talk_boost = boost;
        self
    }

    /// Convenience for tests: build a state with the built-in default
    /// [`AuthConfig`] (closed registration, no anonymous edits, no approval
    /// queue). Production wiring uses [`Self::new`] with the operator-supplied
    /// config.
    #[must_use]
    pub fn new_with_defaults(storage: S) -> Self {
        Self::new(storage, crate::config::Config::defaults().auth)
    }

    /// Attach an [`AuthState`] so the configurable-auth extractors can resolve
    /// session cookies against the auth stack.
    #[must_use]
    pub fn with_auth_state(mut self, auth: AuthState) -> Self {
        self.auth_state = Some(auth);
        self
    }

    /// Override the default page size used by list endpoints.
    #[must_use]
    pub fn with_default_page_size(mut self, n: u32) -> Self {
        self.route_config.default_page_size = n;
        self
    }

    /// Wire the media upload pipeline (#32) — backend + config.
    #[must_use]
    pub fn with_media(
        mut self,
        media_config: crate::config::MediaConfig,
        backend: Arc<dyn MediaBackend>,
    ) -> Self {
        self.media_config = media_config;
        self.media_backend = Some(backend);
        self
    }

    /// Wire the CAPTCHA provider (#41) into the state. Production callers
    /// build the provider once at startup via [`crate::captcha::build_provider`]
    /// and pass it in here so every handler observes the same `Arc<dyn …>`.
    #[must_use]
    pub fn with_captcha(
        mut self,
        captcha_config: CaptchaConfig,
        provider: Arc<dyn CaptchaProvider>,
    ) -> Self {
        self.captcha = provider;
        self.captcha_config = captcha_config;
        self
    }

    /// Attach the blocklist state (#42). The middleware reads from this
    /// shared snapshot; the admin handlers mutate it.
    #[must_use]
    pub fn with_blocklist(mut self, blocklist: BlocklistState) -> Self {
        self.blocklist = Some(blocklist);
        self
    }
}

impl<S: AppStorage> Clone for AppState<S> {
    fn clone(&self) -> Self {
        Self {
            storage: Arc::clone(&self.storage),
            route_config: self.route_config,
            auth_config: self.auth_config.clone(),
            moderation_config: self.moderation_config.clone(),
            auth_state: self.auth_state.clone(),
            search: self.search.clone(),
            searcher: self.searcher.clone(),
            search_title_boost: self.search_title_boost,
            search_talk_boost: self.search_talk_boost,
            media_config: self.media_config.clone(),
            media_backend: self.media_backend.clone(),
            captcha: Arc::clone(&self.captcha),
            captcha_config: self.captcha_config.clone(),
            blocklist: self.blocklist.clone(),
            render_config: self.render_config.clone(),
        }
    }
}

/// Expose the optional [`AuthState`] for axum's `State<AuthState>` extractor.
///
/// Panics if no auth state has been wired — that's a configuration bug at
/// router-construction time, not a per-request failure. Pages handlers go
/// through the configurable-auth extractors (see
/// [`crate::extractors::EditorExtractor`]) which handle the missing-auth case
/// gracefully and treat it as "anonymous caller".
impl<S: AppStorage> FromRef<AppState<S>> for AuthState {
    fn from_ref(input: &AppState<S>) -> Self {
        #[allow(
            clippy::expect_used,
            reason = "router wiring guarantees auth_state is present whenever an auth-state \
                      extractor is reachable; missing it here is a misconfiguration the dev \
                      should see loudly"
        )]
        input
            .auth_state
            .clone()
            .expect("AppState was constructed without an AuthState but a handler requires it")
    }
}
