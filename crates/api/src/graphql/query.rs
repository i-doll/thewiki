//! GraphQL `Query` root.
//!
//! Resolvers delegate to the same repository traits the REST handlers use,
//! so the answer set is identical across the two surfaces. Where REST
//! returns specific DTOs we use the REST handler's *helper* functions
//! (`hydrate_page_view`, `resolve_namespace`, `build_diff`) so the rendering
//! and validation logic stays in one place.

use std::sync::Arc;

use async_graphql::{Context, Error, ErrorExtensions, ID, Object};
use thewiki_core::{NamespaceSlug, Permissions, RevisionId, Username};
use thewiki_storage::StorageError;
use thewiki_storage::repo::{
    AuditLogFilter, AuditLogRepository, Cursor, NamespaceRepository, PageRepository,
    RecentChangesFilter, RecentChangesRepository, RevisionRepository, RoleRepository,
    UserRepository,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::graphql::auth::{current_session, require_permissions};
use crate::graphql::context::GraphQLContext;
use crate::graphql::types::{
    AuditLogConnection, AuditLogEntry, Diff, Page, PageConnection, PageInfo,
    RecentChangeConnection, Revision, RevisionConnection, SearchResults, User,
};
use crate::pages::revisions::{DiffQuery, build_diff_for_handler};
use crate::state::AppStorage;

const DEFAULT_NAMESPACE: &str = "Main";

/// Top-level Query root.
///
/// Generic over the storage facade so the same schema definition fits
/// SQLite (M0) and Postgres / libsql (M1).
pub struct Query<S: AppStorage>(pub std::marker::PhantomData<S>);

impl<S: AppStorage> Default for Query<S> {
    fn default() -> Self {
        Self(std::marker::PhantomData)
    }
}

#[Object]
impl<S: AppStorage> Query<S> {
    /// Fetch a page by slug + namespace.
    ///
    /// `namespace` defaults to `Main`. Returns `null` when the namespace
    /// or page is unknown — clients distinguish "doesn't exist" from
    /// "permission denied" via the absence of an error.
    async fn page(
        &self,
        ctx: &Context<'_>,
        slug: String,
        #[graphql(default = "Main")] namespace: String,
    ) -> Result<Option<Page>, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let state = &gctx.state;
        let ns_slug = NamespaceSlug::new(namespace.clone())
            .map_err(|e| Error::new(format!("namespace_slug: {e}")))?;
        let ns = match state.storage.namespaces().get_by_slug(&ns_slug).await {
            Ok(n) => n,
            Err(StorageError::NotFound) => return Ok(None),
            Err(e) => return Err(storage_error(e)),
        };
        let page = match state
            .storage
            .pages()
            .get_by_namespace_and_slug(ns.id, &slug)
            .await
        {
            Ok(p) => p,
            Err(StorageError::NotFound) => return Ok(None),
            Err(e) => return Err(storage_error(e)),
        };
        let view = crate::pages::routes::hydrate_page_view(state, page, ns.slug.into_string())
            .await
            .map_err(api_error)?;
        Ok(Some(Page::from_view(view)))
    }

    /// List pages in a namespace, cursor-paginated.
    async fn pages(
        &self,
        ctx: &Context<'_>,
        cursor: Option<String>,
        #[graphql(default = 50)] limit: u32,
        #[graphql(default = "Main")] namespace: String,
    ) -> Result<PageConnection, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let state = &gctx.state;
        let ns_slug = NamespaceSlug::new(namespace.clone())
            .map_err(|e| Error::new(format!("namespace_slug: {e}")))?;
        let ns = state
            .storage
            .namespaces()
            .get_by_slug(&ns_slug)
            .await
            .map_err(storage_error)?;
        let ns_label = ns.slug.as_str().to_owned();
        let slice = state
            .storage
            .pages()
            .list_in_namespace(ns.id, cursor.map(Cursor), limit)
            .await
            .map_err(storage_error)?;
        let items = slice
            .items
            .into_iter()
            .map(|p| Page::from_core_summary(p, ns_label.clone()))
            .collect();
        Ok(PageConnection {
            items,
            page_info: PageInfo::from_next(slice.next),
        })
    }

    /// Fetch a revision by id.
    async fn revision(&self, ctx: &Context<'_>, id: ID) -> Result<Option<Revision>, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let rev_id = parse_revision_id(&id)?;
        match gctx.state.storage.revisions().get_by_id(rev_id).await {
            Ok(r) => Ok(Some(r.into())),
            Err(StorageError::NotFound) => Ok(None),
            Err(e) => Err(storage_error(e)),
        }
    }

    /// List revisions for a page, newest first.
    async fn revisions(
        &self,
        ctx: &Context<'_>,
        page_slug: String,
        cursor: Option<String>,
        #[graphql(default = 50)] limit: u32,
    ) -> Result<RevisionConnection, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let state = &gctx.state;
        let ns_slug = NamespaceSlug::new(DEFAULT_NAMESPACE)
            .map_err(|e| Error::new(format!("namespace_slug: {e}")))?;
        let ns = state
            .storage
            .namespaces()
            .get_by_slug(&ns_slug)
            .await
            .map_err(storage_error)?;
        let page = state
            .storage
            .pages()
            .get_by_namespace_and_slug(ns.id, &page_slug)
            .await
            .map_err(storage_error)?;
        let slice = state
            .storage
            .revisions()
            .list_for_page(page.id, cursor.map(Cursor), limit)
            .await
            .map_err(storage_error)?;
        Ok(RevisionConnection {
            items: slice.items.into_iter().map(Into::into).collect(),
            page_info: PageInfo::from_next(slice.next),
        })
    }

    /// Compute a pairwise diff between two revisions.
    ///
    /// Both revisions must belong to the same page; a mismatch returns an
    /// error (we don't reveal cross-page revision ids).
    async fn diff(&self, ctx: &Context<'_>, from: ID, to: ID) -> Result<Diff, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let state = &gctx.state;
        let from_id = parse_revision_id(&from)?;
        let to_id = parse_revision_id(&to)?;
        let from_rev = state
            .storage
            .revisions()
            .get_by_id(from_id)
            .await
            .map_err(storage_error)?;
        let to_rev = state
            .storage
            .revisions()
            .get_by_id(to_id)
            .await
            .map_err(storage_error)?;
        if from_rev.page_id != to_rev.page_id {
            return Err(Error::new("revisions belong to different pages"));
        }
        let diff = build_diff_for_handler(
            DiffQuery {
                from: from_id,
                to: to_id,
            },
            &from_rev.body,
            &to_rev.body,
        );
        Ok(diff.into())
    }

    /// Wiki-wide recent-changes feed.
    async fn recent_changes(
        &self,
        ctx: &Context<'_>,
        since: Option<OffsetDateTime>,
        namespace: Option<String>,
        actor: Option<String>,
        cursor: Option<String>,
        #[graphql(default = 50)] limit: u32,
    ) -> Result<RecentChangeConnection, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let state = &gctx.state;

        // Resolve namespace filter; unknown → typed error so the client
        // sees the mismatch rather than "list is empty".
        let namespace_id = match namespace.as_deref() {
            Some(raw) => {
                let slug = NamespaceSlug::new(raw)
                    .map_err(|err| Error::new(format!("namespace: {err}")))?;
                let ns = state
                    .storage
                    .namespaces()
                    .get_by_slug(&slug)
                    .await
                    .map_err(storage_error)?;
                Some(ns.id)
            }
            None => None,
        };
        let actor_id = match actor.as_deref() {
            Some(raw) => {
                let u = Username::new(raw).map_err(|e| Error::new(format!("actor: {e}")))?;
                let user = state
                    .storage
                    .users()
                    .get_by_username(&u)
                    .await
                    .map_err(storage_error)?;
                Some(user.id)
            }
            None => None,
        };

        let slice = state
            .storage
            .recent_changes()
            .list(
                RecentChangesFilter {
                    since,
                    namespace_id,
                    actor_id,
                },
                cursor.map(Cursor),
                limit,
            )
            .await
            .map_err(storage_error)?;
        Ok(RecentChangeConnection {
            items: slice.items.into_iter().map(Into::into).collect(),
            page_info: PageInfo::from_next(slice.next),
        })
    }

    /// Full-text search.
    ///
    /// Returns up to `limit` hits ordered by BM25 score. The search index
    /// is updated asynchronously after every page edit (eventually
    /// consistent, ~200 ms lag); freshly-committed content may not be
    /// immediately discoverable.
    async fn search(
        &self,
        ctx: &Context<'_>,
        query: String,
        namespace: Option<String>,
        #[graphql(default = 10)] limit: u32,
    ) -> Result<SearchResults, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let state = &gctx.state;
        if !state.searcher.is_enabled() {
            // Search is disabled in this deployment / test fixture; return
            // an empty result rather than a misleading error.
            return Ok(SearchResults {
                hits: Vec::new(),
                total_estimate: 0,
            });
        }
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(SearchResults {
                hits: Vec::new(),
                total_estimate: 0,
            });
        }
        // Mirror the REST limit-clamp so REST + GraphQL behave identically.
        let limit = match limit {
            0 => crate::search::routes::DEFAULT_LIMIT,
            n => n.min(crate::search::routes::MAX_LIMIT),
        };
        let sq = thewiki_search::SearchQuery {
            text: trimmed.to_string(),
            namespace_id: None,
            namespace_slug: namespace,
            tag: None,
            limit,
            cursor: None,
            title_boost: state.search_title_boost,
        };
        let results = state
            .searcher
            .search(&sq)
            .map_err(|e| Error::new(format!("search: {e}")))?;
        Ok(SearchResults::from(results))
    }

    /// Administrative audit log. Requires the `VIEW_AUDIT_LOG` permission.
    #[allow(
        clippy::too_many_arguments,
        reason = "GraphQL field arguments — one per filter"
    )]
    async fn audit_log(
        &self,
        ctx: &Context<'_>,
        action: Option<String>,
        actor: Option<ID>,
        since: Option<OffsetDateTime>,
        until: Option<OffsetDateTime>,
        cursor: Option<String>,
        #[graphql(default = 50)] limit: u32,
    ) -> Result<AuditLogConnection, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let _user = require_permissions(ctx, Permissions::VIEW_AUDIT_LOG)?;

        // The REST endpoint filters by actor *username*; the GraphQL surface
        // accepts the actor id (which is more useful for tool integrations).
        // Resolve the id to a username at the boundary so the underlying
        // filter (which still keys on username for retention reasons) keeps
        // working unchanged.
        let actor_username = match actor.as_deref() {
            Some(raw) => {
                let uuid = Uuid::parse_str(raw)
                    .map_err(|e| Error::new(format!("actor id is not a valid uuid: {e}")))?;
                let uid = thewiki_core::UserId::from_uuid(uuid);
                match gctx.state.storage.users().get_by_id(uid).await {
                    Ok(u) => Some(u.username.as_str().to_owned()),
                    Err(StorageError::NotFound) => {
                        return Ok(AuditLogConnection {
                            items: Vec::new(),
                            page_info: PageInfo::from_next(None),
                        });
                    }
                    Err(e) => return Err(storage_error(e)),
                }
            }
            None => None,
        };

        if let (Some(since_v), Some(until_v)) = (since, until)
            && since_v > until_v
        {
            return Err(Error::new("since must be at or before until"));
        }
        let slice = gctx
            .state
            .storage
            .audit_log()
            .list(
                AuditLogFilter {
                    actor_username,
                    action,
                    since,
                    until,
                },
                cursor.map(Cursor),
                limit,
            )
            .await
            .map_err(storage_error)?;
        let items: Vec<AuditLogEntry> = slice.items.into_iter().map(Into::into).collect();
        Ok(AuditLogConnection {
            items,
            page_info: PageInfo::from_next(slice.next),
        })
    }

    /// Current authenticated user, or `null` for anonymous callers.
    async fn me(&self, ctx: &Context<'_>) -> Result<Option<User>, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let session = current_session(ctx);
        let Some(user) = session.user.clone() else {
            return Ok(None);
        };
        // Role-set lookup goes through the auth_state's storage handle (which
        // is the concrete SQLite facade exposing `roles()`). The generic
        // `AppStorage` trait deliberately keeps role mgmt off its surface
        // because the page handlers don't need it.
        let Some(auth_state) = gctx.state.auth_state.as_ref() else {
            // No auth stack wired — surface user with no roles. Tests that
            // bypass the auth stack still get a deterministic answer.
            return Ok(Some(User::from_parts(user, &[], session.permissions)));
        };
        let roles = auth_state
            .storage
            .roles()
            .list_for_user(user.id)
            .await
            .map_err(storage_error)?;
        Ok(Some(User::from_parts(user, &roles, session.permissions)))
    }
}

fn ctx_storage<'a, S: AppStorage>(ctx: &'a Context<'_>) -> Result<&'a GraphQLContext<S>, Error> {
    ctx.data::<GraphQLContext<S>>().map_err(|_| {
        Error::new("missing GraphQLContext in resolver data")
            .extend_with(|_, e| e.set("code", "INTERNAL"))
    })
}

fn parse_revision_id(raw: &ID) -> Result<RevisionId, Error> {
    Uuid::parse_str(raw.as_str())
        .map(RevisionId::from_uuid)
        .map_err(|e| Error::new(format!("revision id is not a valid uuid: {e}")))
}

fn storage_error(err: StorageError) -> Error {
    let code = match &err {
        StorageError::NotFound => "NOT_FOUND",
        StorageError::Conflict(_) => "CONFLICT",
        StorageError::InvalidInput(_) => "INVALID_INPUT",
        _ => "INTERNAL",
    };
    Error::new(err.to_string()).extend_with(|_, e| e.set("code", code))
}

fn api_error(err: crate::error::ApiError) -> Error {
    let code = err.code().to_uppercase();
    Error::new(err.to_string()).extend_with(|_, e| e.set("code", code))
}

// Re-export `Arc` so the schema file's bound checks are satisfied without
// re-importing it everywhere.
#[allow(dead_code)]
fn _ensure_arc_in_scope() -> Arc<()> {
    Arc::new(())
}
