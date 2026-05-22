//! GraphQL surface (#37).
//!
//! Mirrors the REST coverage at `/api/graphql`. Sub-routes:
//!
//! - `POST /api/graphql` — main query / mutation endpoint.
//! - `GET /api/graphql/playground` — GraphiQL HTML (gated by config).
//! - `GET /api/graphql/schema` — schema definition language for tooling.
//!
//! The endpoint resolves the `thewiki_session` cookie before invoking the
//! schema so resolvers can read the caller's session through
//! [`auth::SessionContext`]. Mutations honour the operator-configured
//! `auth.anonymous_edits` flag — see `mutation.rs` for the exact
//! per-resolver gates.

pub mod auth;
pub mod context;
pub mod mutation;
pub mod query;
pub mod schema;
pub mod types;

use std::sync::Arc;

use async_graphql::http::{GraphQLPlaygroundConfig, GraphiQLSource, playground_source};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::Router;
use axum::extract::{FromRef, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use thewiki_core::{Permissions, SessionId};
use thewiki_storage::StorageError;
use thewiki_storage::repo::{RoleRepository, SessionRepository, UserRepository};
use tower_cookies::Cookies;

use crate::auth::AuthState;
use crate::auth::session::{SESSION_COOKIE, decode_session_id};
use crate::config::GraphQLConfig;
use crate::graphql::auth::SessionContext;
use crate::graphql::context::GraphQLContext;
use crate::graphql::schema::{AppSchema, build};
use crate::state::{AppState, AppStorage};

/// Per-route slice of GraphQL state. Lives in axum's `State<…>`.
///
/// Holds the pre-built schema (cheap to clone) and the operator config
/// so handlers can decide whether to surface the GraphiQL page.
pub struct GraphQLState<S: AppStorage> {
    pub schema: AppSchema<S>,
    pub config: GraphQLConfig,
    pub app_state: Arc<AppState<S>>,
}

impl<S: AppStorage> Clone for GraphQLState<S> {
    fn clone(&self) -> Self {
        Self {
            schema: self.schema.clone(),
            config: self.config.clone(),
            app_state: Arc::clone(&self.app_state),
        }
    }
}

impl<S: AppStorage> GraphQLState<S> {
    /// Construct a state with a freshly-built schema.
    #[must_use]
    pub fn new(app_state: AppState<S>, config: GraphQLConfig) -> Self {
        let schema = build::<S>(&config);
        Self {
            schema,
            config,
            app_state: Arc::new(app_state),
        }
    }
}

impl<S: AppStorage> FromRef<GraphQLState<S>> for AppState<S> {
    fn from_ref(input: &GraphQLState<S>) -> Self {
        (*input.app_state).clone()
    }
}

/// Build the `/api/graphql` subrouter.
///
/// Mounted by `crate::app::build_full` under `/api/graphql`. The router
/// holds a `GraphQLState<S>` directly rather than going through the
/// `AppState<S>` indirection — the schema needs a stable handle anyway,
/// and giving it its own state type keeps the GraphQL boundary explicit.
pub fn router<S: AppStorage>() -> Router<GraphQLState<S>> {
    Router::new()
        .route("/api/graphql", post(graphql_handler::<S>))
        .route("/api/graphql/playground", get(playground_handler::<S>))
        .route("/api/graphql/schema", get(schema_handler::<S>))
}

/// `POST /api/graphql` — primary GraphQL endpoint.
///
/// Resolves the session cookie (when present) and the `AuthSession` context
/// before invoking the schema. Anonymous requests pass through with
/// `SessionContext::anonymous`.
pub async fn graphql_handler<S: AppStorage>(
    State(state): State<GraphQLState<S>>,
    cookies: Cookies,
    req: GraphQLRequest,
) -> GraphQLResponse {
    let session = resolve_session(&state, &cookies).await;
    let gql_ctx = GraphQLContext::new(Arc::clone(&state.app_state));
    let request = req.into_inner().data(session).data(gql_ctx);
    state.schema.execute(request).await.into()
}

/// `GET /api/graphql/playground` — GraphiQL UI.
///
/// Returns 404 when `graphql.playground_enabled = false`.
pub async fn playground_handler<S: AppStorage>(State(state): State<GraphQLState<S>>) -> Response {
    if !state.config.playground_enabled {
        return (StatusCode::NOT_FOUND, "playground disabled").into_response();
    }
    let html = GraphiQLSource::build()
        .endpoint("/api/graphql")
        .title("thewiki GraphQL")
        .finish();
    Html(html).into_response()
}

/// `GET /api/graphql/schema` — emit the schema in SDL form.
///
/// Useful for tooling (Apollo codegen, schema diffing in CI). Disabled
/// when introspection is disabled — exposing the SDL would defeat the
/// point of turning introspection off.
pub async fn schema_handler<S: AppStorage>(State(state): State<GraphQLState<S>>) -> Response {
    if !state.config.introspection_enabled {
        return (StatusCode::NOT_FOUND, "schema endpoint disabled").into_response();
    }
    let sdl = state.schema.sdl();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        sdl,
    )
        .into_response()
}

/// Resolve the `thewiki_session` cookie into a `SessionContext`.
///
/// Mirrors `AuthSession::from_request_parts` but never returns an error:
/// any cookie misstep (missing, malformed, expired) yields an anonymous
/// context. Mutations that require auth surface their own 401 inside the
/// resolver via `require_session`.
async fn resolve_session<S: AppStorage>(
    state: &GraphQLState<S>,
    cookies: &Cookies,
) -> SessionContext {
    let Some(auth_state) = state.app_state.auth_state.as_ref() else {
        return SessionContext::anonymous();
    };
    let Some(session_id) = cookies
        .get(SESSION_COOKIE)
        .and_then(|c| decode_session_id(c.value()))
    else {
        return SessionContext::anonymous();
    };
    match load_session(auth_state, session_id).await {
        Ok(Some(ctx)) => ctx,
        Ok(None) | Err(_) => SessionContext::anonymous(),
    }
}

async fn load_session(
    auth_state: &AuthState,
    session_id: SessionId,
) -> Result<Option<SessionContext>, StorageError> {
    let session = match auth_state.storage.sessions().get_by_id(session_id).await {
        Ok(s) => s,
        Err(StorageError::NotFound) => return Ok(None),
        Err(e) => return Err(e),
    };
    let user = match auth_state.storage.users().get_by_id(session.user_id).await {
        Ok(u) => u,
        Err(StorageError::NotFound) => return Ok(None),
        Err(e) => return Err(e),
    };
    // Touch best-effort; failure here doesn't fail the request.
    let _ = auth_state.storage.sessions().touch(session.id).await;
    let roles = auth_state.storage.roles().list_for_user(user.id).await?;
    let permissions = roles
        .into_iter()
        .fold(Permissions::empty(), |acc, r| acc | r.permissions);
    Ok(Some(SessionContext::authenticated(user, permissions)))
}

#[allow(unused_imports)]
use playground_source as _classic_playground; // keep symbol reachable

#[allow(dead_code)]
fn _ensure_classic_playground_ok() {
    let _ = playground_source(GraphQLPlaygroundConfig::new("/api/graphql"));
}
