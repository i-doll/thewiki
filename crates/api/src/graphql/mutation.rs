//! GraphQL `Mutation` root.
//!
//! Mutations route to the same domain calls as the REST handlers. Auth is
//! gated by the resolver-side `require_session` / `require_user_or_anonymous`
//! helpers — the cookie has already been resolved by the HTTP handler before
//! the schema is invoked.
//!
//! The `uploadMedia` mutation is **deliberately omitted**: GraphQL's
//! multipart spec isn't standardised in async-graphql 7 (it requires a
//! dedicated transport-level shim), and the surface we'd expose would be
//! either a base64-encoded string (poor fit for binary uploads) or a
//! second request to the REST endpoint anyway. We document the gap on the
//! schema and direct clients to `POST /api/v1/media` for the upload flow.

use std::sync::Arc;

use async_graphql::{Context, Error, ErrorExtensions, ID, Object};
use serde_json::json;
use thewiki_core::{
    ContentFormat, NamespaceSlug, Page as CorePage, PageId, ProtectionLevel,
    Revision as CoreRevision, RevisionId, User as CoreUser, Username,
};
use thewiki_search::PageDoc;
use thewiki_storage::StorageError;
use thewiki_storage::repo::{
    NamespaceRepository, PageAuditMutation, PageRepository, RevisionRepository, RoleRepository,
    SessionRepository, UserRepository,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::password::PasswordHasher;
use crate::graphql::auth::{require_session, require_user_or_anonymous, unauthenticated_error};
use crate::graphql::context::GraphQLContext;
use crate::graphql::types::{LoginPayload, Page, User};
use crate::pages::audit::page_event;
use crate::state::AppStorage;

const DEFAULT_NAMESPACE: &str = "Main";

/// Top-level Mutation root.
pub struct Mutation<S: AppStorage>(pub std::marker::PhantomData<S>);

impl<S: AppStorage> Default for Mutation<S> {
    fn default() -> Self {
        Self(std::marker::PhantomData)
    }
}

#[Object]
impl<S: AppStorage> Mutation<S> {
    /// Create a page + its initial revision.
    ///
    /// Honours `auth.anonymous_edits`: anonymous callers are accepted when
    /// the operator has enabled it (and the edit is credited to the
    /// singleton anonymous user); otherwise the mutation fails with
    /// `UNAUTHENTICATED`.
    async fn create_page(
        &self,
        ctx: &Context<'_>,
        namespace_slug: String,
        slug: String,
        title: String,
        content: String,
    ) -> Result<Page, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let state = &gctx.state;
        let (user_id, username, _is_anonymous) =
            require_user_or_anonymous(ctx, state.auth_config.anonymous_edits)?;

        if slug.trim().is_empty() {
            return Err(invalid_input("slug must not be empty"));
        }
        if title.trim().is_empty() {
            return Err(invalid_input("title must not be empty"));
        }

        let ns_slug = NamespaceSlug::new(namespace_slug)
            .map_err(|e| invalid_input(&format!("namespace_slug: {e}")))?;
        let namespace = state
            .storage
            .namespaces()
            .get_by_slug(&ns_slug)
            .await
            .map_err(storage_error)?;
        let ns_label = namespace.slug.as_str().to_owned();

        let now = OffsetDateTime::now_utc();
        let mut page = CorePage {
            id: PageId::new(),
            namespace_id: namespace.id,
            slug,
            title,
            current_revision_id: None,
            content_format: ContentFormat::Markdown,
            protection_level: ProtectionLevel::None,
            created_at: now,
            updated_at: now,
        };
        let revision = CoreRevision::new(page.id, None, user_id, content, None);
        page.current_revision_id = Some(revision.id);
        page.updated_at = OffsetDateTime::now_utc();

        let audit = page_event(
            user_id,
            &username,
            "page.create",
            page.id,
            format!("{ns_label}/{}", page.slug),
            json!({
                "namespace": ns_label,
                "slug": page.slug.as_str(),
                "live": true,
                "revision_id": revision.id.into_uuid(),
                "via": "graphql",
            }),
        );
        let body_snapshot = revision.body.clone();
        state
            .storage
            .commit_page_audit(
                PageAuditMutation::CreatePage {
                    page: page.clone(),
                    live_revision: Some(revision),
                },
                audit,
            )
            .await
            .map_err(storage_error)?;

        // Mirror the REST handler's eventually-consistent search upsert.
        state.search.upsert(PageDoc {
            page_id: page.id,
            namespace_id: page.namespace_id,
            namespace_slug: ns_label.clone(),
            slug: page.slug.clone(),
            title: page.title.clone(),
            body: body_snapshot,
            tags: Vec::new(),
            updated_at: page.updated_at,
        });

        let view = crate::pages::routes::hydrate_page_view(state, page, ns_label)
            .await
            .map_err(api_error)?;
        Ok(Page::from_view(view))
    }

    /// Commit a new revision to an existing page.
    async fn update_page(
        &self,
        ctx: &Context<'_>,
        slug: String,
        title: Option<String>,
        content: String,
        edit_summary: Option<String>,
    ) -> Result<Page, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let state = &gctx.state;
        let (user_id, username, _is_anonymous) =
            require_user_or_anonymous(ctx, state.auth_config.anonymous_edits)?;

        if let Some(t) = title.as_deref()
            && t.trim().is_empty()
        {
            return Err(invalid_input("title must not be empty"));
        }

        let ns_slug = NamespaceSlug::new(DEFAULT_NAMESPACE)
            .map_err(|e| invalid_input(&format!("namespace_slug: {e}")))?;
        let namespace = state
            .storage
            .namespaces()
            .get_by_slug(&ns_slug)
            .await
            .map_err(storage_error)?;
        let ns_label = namespace.slug.as_str().to_owned();
        let mut page = state
            .storage
            .pages()
            .get_by_namespace_and_slug(namespace.id, &slug)
            .await
            .map_err(storage_error)?;
        let revision = CoreRevision::new(
            page.id,
            page.current_revision_id,
            user_id,
            content,
            edit_summary,
        );
        if let Some(t) = title {
            page.title = t;
        }
        page.current_revision_id = Some(revision.id);
        page.updated_at = OffsetDateTime::now_utc();

        let audit = page_event(
            user_id,
            &username,
            "page.update",
            page.id,
            format!("{ns_label}/{}", page.slug),
            json!({
                "namespace": ns_label,
                "slug": page.slug.as_str(),
                "live": true,
                "revision_id": revision.id.into_uuid(),
                "via": "graphql",
            }),
        );
        let body_snapshot = revision.body.clone();
        state
            .storage
            .commit_page_audit(
                PageAuditMutation::CommitRevision {
                    page: page.clone(),
                    revision,
                },
                audit,
            )
            .await
            .map_err(storage_error)?;

        state.search.upsert(PageDoc {
            page_id: page.id,
            namespace_id: page.namespace_id,
            namespace_slug: ns_label.clone(),
            slug: page.slug.clone(),
            title: page.title.clone(),
            body: body_snapshot,
            tags: Vec::new(),
            updated_at: page.updated_at,
        });

        let view = crate::pages::routes::hydrate_page_view(state, page, ns_label)
            .await
            .map_err(api_error)?;
        Ok(Page::from_view(view))
    }

    /// Delete a page (cascades to its revisions).
    async fn delete_page(&self, ctx: &Context<'_>, slug: String) -> Result<bool, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let state = &gctx.state;
        let (user_id, username, _is_anonymous) =
            require_user_or_anonymous(ctx, state.auth_config.anonymous_edits)?;

        let ns_slug = NamespaceSlug::new(DEFAULT_NAMESPACE)
            .map_err(|e| invalid_input(&format!("namespace_slug: {e}")))?;
        let namespace = state
            .storage
            .namespaces()
            .get_by_slug(&ns_slug)
            .await
            .map_err(storage_error)?;
        let ns_label = namespace.slug.as_str().to_owned();
        let page = state
            .storage
            .pages()
            .get_by_namespace_and_slug(namespace.id, &slug)
            .await
            .map_err(storage_error)?;
        let audit = page_event(
            user_id,
            &username,
            "page.delete",
            page.id,
            format!("{ns_label}/{}", page.slug),
            json!({
                "namespace": ns_label,
                "slug": page.slug.as_str(),
                "via": "graphql",
            }),
        );
        state
            .storage
            .commit_page_audit(PageAuditMutation::DeletePage { page_id: page.id }, audit)
            .await
            .map_err(storage_error)?;
        state.search.delete(page.id);
        Ok(true)
    }

    /// Revert a page to a historical revision.
    ///
    /// Requires an authenticated session — anonymous reverts are not
    /// permitted (the REST endpoint has the same posture).
    async fn revert_page(
        &self,
        ctx: &Context<'_>,
        slug: String,
        from_revision_id: ID,
        message: Option<String>,
    ) -> Result<Page, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let state = &gctx.state;
        let user = require_session(ctx)?;
        let user_id = user.id;
        let username = user.username.as_str().to_owned();

        let from_rev_id = parse_revision_id(&from_revision_id)?;
        let ns_slug = NamespaceSlug::new(DEFAULT_NAMESPACE)
            .map_err(|e| invalid_input(&format!("namespace_slug: {e}")))?;
        let namespace = state
            .storage
            .namespaces()
            .get_by_slug(&ns_slug)
            .await
            .map_err(storage_error)?;
        let ns_label = namespace.slug.as_str().to_owned();
        let mut page = state
            .storage
            .pages()
            .get_by_namespace_and_slug(namespace.id, &slug)
            .await
            .map_err(storage_error)?;
        let historical = state
            .storage
            .revisions()
            .get_by_id(from_rev_id)
            .await
            .map_err(storage_error)?;
        if historical.page_id != page.id {
            return Err(Error::new("revision does not belong to this page")
                .extend_with(|_, e| e.set("code", "NOT_FOUND")));
        }
        let edit_summary = message
            .filter(|m| !m.trim().is_empty())
            .or_else(|| Some(format!("Reverted to {}", historical.id)));
        let new_revision = CoreRevision::new(
            page.id,
            page.current_revision_id,
            user_id,
            historical.body.clone(),
            edit_summary,
        );
        page.current_revision_id = Some(new_revision.id);
        page.updated_at = OffsetDateTime::now_utc();

        let audit = page_event(
            user_id,
            &username,
            "page.revert",
            page.id,
            format!("{ns_label}/{}", page.slug),
            json!({
                "namespace": ns_label,
                "slug": page.slug.as_str(),
                "from_revision_id": historical.id.into_uuid(),
                "new_revision_id": new_revision.id.into_uuid(),
                "via": "graphql",
            }),
        );
        let body_snapshot = new_revision.body.clone();
        state
            .storage
            .commit_page_audit(
                PageAuditMutation::CommitRevision {
                    page: page.clone(),
                    revision: new_revision,
                },
                audit,
            )
            .await
            .map_err(storage_error)?;

        state.search.upsert(PageDoc {
            page_id: page.id,
            namespace_id: page.namespace_id,
            namespace_slug: ns_label.clone(),
            slug: page.slug.clone(),
            title: page.title.clone(),
            body: body_snapshot,
            tags: Vec::new(),
            updated_at: page.updated_at,
        });

        let view = crate::pages::routes::hydrate_page_view(state, page, ns_label)
            .await
            .map_err(api_error)?;
        Ok(Page::from_view(view))
    }

    /// Authenticate via username + password and issue a session cookie.
    ///
    /// **Note on the cookie**: the GraphQL endpoint doesn't have direct
    /// access to the response's `Set-Cookie` header from inside a resolver.
    /// Today the auth flow goes through `POST /api/v1/auth/login`, which
    /// sets the cookies and returns the user payload. The GraphQL `login`
    /// mutation surface is here for clients that want a uniform GraphQL-only
    /// flow and is wired against the same hasher / session machinery — when
    /// it returns the user payload, the SPA should subsequently call the
    /// REST endpoint to materialise the cookie (or, alternatively, this
    /// mutation can be invoked through the same axum handler that injects
    /// cookies, which is supported by the dispatch in `graphql/mod.rs`).
    async fn login(
        &self,
        ctx: &Context<'_>,
        username: String,
        password: String,
    ) -> Result<LoginPayload, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let state = &gctx.state;
        let Some(auth_state) = state.auth_state.as_ref() else {
            return Err(Error::new("authentication is not configured")
                .extend_with(|_, e| e.set("code", "UNAVAILABLE")));
        };
        let parsed = Username::new(username).map_err(|_| {
            Error::new("invalid_credentials").extend_with(|_, e| e.set("code", "UNAUTHENTICATED"))
        })?;
        let user = match auth_state.storage.users().get_by_username(&parsed).await {
            Ok(u) => u,
            Err(StorageError::NotFound) => {
                return Err(Error::new("invalid_credentials")
                    .extend_with(|_, e| e.set("code", "UNAUTHENTICATED")));
            }
            Err(e) => return Err(storage_error(e)),
        };

        // Fetch hash and verify. Mirrors the timing-safe paths in the REST
        // login handler.
        let phc = fetch_password_hash(state, &user).await?;
        let dummy = auth_state
            .hasher
            .dummy_hash_for_timing()
            .map_err(hash_err)?;
        let hash_to_check = phc.as_deref().unwrap_or(&dummy);
        let ok = auth_state
            .hasher
            .verify(&password, hash_to_check)
            .map_err(hash_err)?;
        if !ok || phc.is_none() {
            return Err(Error::new("invalid_credentials")
                .extend_with(|_, e| e.set("code", "UNAUTHENTICATED")));
        }

        // Issue a session row. The cookie itself is set by the GraphQL
        // route handler (see `graphql/mod.rs`) when this mutation is
        // invoked through that path. Resolvers can't directly write
        // headers; the handler post-processes the response.
        let session = auth_state
            .storage
            .sessions()
            .create(user.id, auth_state.session_ttl, None, None)
            .await
            .map_err(storage_error)?;
        ctx.insert_http_header(
            "x-thewiki-session-issued",
            session.id.into_uuid().to_string(),
        );

        let roles = auth_state
            .storage
            .roles()
            .list_for_user(user.id)
            .await
            .map_err(storage_error)?;
        let permissions = roles
            .iter()
            .fold(thewiki_core::Permissions::empty(), |acc, r| {
                acc | r.permissions
            });
        Ok(LoginPayload {
            user: User::from_parts(user, &roles, permissions),
        })
    }

    /// Log out of the current session. Returns `true` if a session was
    /// revoked, `false` if the request was already anonymous.
    async fn logout(&self, ctx: &Context<'_>) -> Result<bool, Error> {
        let gctx = ctx_storage::<S>(ctx)?;
        let state = &gctx.state;
        let session = crate::graphql::auth::current_session(ctx);
        let Some(user) = session.user.as_ref() else {
            return Ok(false);
        };
        let Some(auth_state) = state.auth_state.as_ref() else {
            return Err(Error::new("authentication is not configured")
                .extend_with(|_, e| e.set("code", "UNAVAILABLE")));
        };
        // We don't have the session id here directly — the GraphQL context
        // intentionally doesn't include it (it's a cookie-bound secret).
        // Pulling sessions by user_id + revoking the most recent one is
        // ambiguous if the user has multiple devices. Today the logout
        // mutation is best-effort: it deletes every session the user holds
        // so the result is "this account is logged out everywhere", which
        // matches a "Log out of all sessions" UI affordance.
        let sessions = auth_state.storage.sessions().delete_for_user(user.id).await;
        match sessions {
            Ok(_) | Err(StorageError::NotFound) => Ok(true),
            Err(e) => Err(storage_error(e)),
        }
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
        .map_err(|e| invalid_input(&format!("revision id is not a valid uuid: {e}")))
}

fn invalid_input(msg: &str) -> Error {
    Error::new(msg.to_string()).extend_with(|_, e| e.set("code", "INVALID_INPUT"))
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

fn hash_err(e: crate::auth::error::AuthError) -> Error {
    Error::new(e.to_string()).extend_with(|_, ext| ext.set("code", "INTERNAL"))
}

async fn fetch_password_hash<S: AppStorage>(
    state: &crate::state::AppState<S>,
    user: &CoreUser,
) -> Result<Option<String>, Error> {
    use crate::auth::error::AuthError;
    let auth_state = state
        .auth_state
        .as_ref()
        .ok_or_else(unauthenticated_error)?;
    // The PHC string lives on a column the user repository doesn't expose;
    // the REST handler reads it via a raw query. Replicate that here.
    let id_bytes = *user.id.as_uuid().as_bytes();
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT password_hash FROM users WHERE id = ?1")
            .bind(id_bytes.as_slice())
            .fetch_optional(auth_state.storage.pool())
            .await
            .map_err(|e| hash_err(AuthError::Storage(StorageError::Database(e))))?;
    Ok(row.and_then(|(h,)| h))
}

// Keep `Arc` in scope so the type-bound check on `GraphQLContext` passes
// the dead-code-warning gate.
#[allow(dead_code)]
fn _ensure_arc_in_scope() -> Arc<()> {
    Arc::new(())
}
