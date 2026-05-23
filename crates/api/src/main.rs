//! `thewiki` binary entrypoint.
//!
//! Parses the CLI, initialises structured logging, and dispatches to the
//! requested subcommand. All real wiring lives in the library; this file is
//! intentionally trivial.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use thewiki_api::auth::password::Argon2Hasher;
use thewiki_api::auth::state::AuthState;
use thewiki_api::rate_limit::RateLimitState;
use thewiki_api::{
    app,
    captcha as captcha_module,
    cli::{self, ConfigCommand, RegenThumbnailsArgs, ReindexArgs},
    config::Config,
    media, telemetry,
};
use thewiki_search::{Indexer, PageDoc, SearchIndex, Searcher};
use thewiki_storage::repo::{
    AuditLogRepository, Cursor, MediaRepository, NamespaceRepository, PageRepository,
    RevisionRepository,
};
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::{Duration as TimeDuration, OffsetDateTime};
use tokio::time::MissedTickBehavior;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<ExitCode> {
    let cli = cli::Cli::parse();
    telemetry::init();

    match cli.command {
        cli::Command::Serve(args) => serve(args).await.map(|()| ExitCode::SUCCESS),
        cli::Command::Openapi => run_openapi(),
        cli::Command::Config(cmd) => Ok(run_config(cmd)),
        cli::Command::Reindex(args) => run_reindex(args).await.map(|()| ExitCode::SUCCESS),
        cli::Command::RegenThumbnails(args) => {
            run_regen_thumbnails(args).await.map(|()| ExitCode::SUCCESS)
        }
    }
}

/// Emit the generated OpenAPI document. This is used by CI to ensure
/// `docs/openapi.json` stays in sync with handler annotations.
fn run_openapi() -> anyhow::Result<ExitCode> {
    let doc = app::openapi::<SqliteStorage>();
    let json = serde_json::to_string_pretty(&doc).context("serialising OpenAPI document")?;
    println!("{json}");
    Ok(ExitCode::SUCCESS)
}

async fn serve(args: cli::ServeArgs) -> anyhow::Result<()> {
    let config = Config::load(args.config.as_deref()).context("loading configuration")?;
    config.validate().context("validating configuration")?;

    // Open storage + apply migrations. The pool stays alive for the lifetime
    // of the process.
    let storage = SqliteStorage::new(
        &config.database.url,
        SqliteOptions {
            max_connections: config.database.max_connections,
            acquire_timeout: Duration::from_secs(config.database.acquire_timeout_secs),
            foreign_keys: true,
        },
    )
    .await
    .with_context(|| format!("opening storage at {}", config.database.url))?;

    // Seed the implicit `Main` namespace (#28). Idempotent — a no-op on
    // every boot after the first. Runs before the HTTP listener binds so
    // a fresh database is never served a 404 for the default namespace.
    let main_ns = storage
        .namespaces()
        .get_or_create_default()
        .await
        .context("seeding default namespace")?;
    tracing::info!(
        slug = %main_ns.slug.as_str(),
        id = %main_ns.id.into_uuid(),
        "default namespace ready",
    );

    if config.audit_log.enabled {
        let pruned = prune_expired_audit_log(&storage, config.audit_log.retention_days)
            .await
            .context("pruning expired audit log rows")?;
        if pruned > 0 {
            info!(rows = pruned, "pruned expired audit log rows");
        }
        spawn_audit_log_pruner(storage.clone(), config.audit_log.retention_days);
    } else {
        info!("audit_log.enabled = false: skipping background pruner");
    }

    let hasher = Arc::new(Argon2Hasher::new(config.auth.argon2).context("building argon2 hasher")?);
    let session_ttl = Duration::from_secs(u64::from(config.auth.session_ttl_hours) * 3600);
    let secure_cookies = !args.insecure_cookie;
    if !secure_cookies {
        tracing::warn!(
            "running with --insecure-cookie: session cookies omit the Secure flag — \
             local dev only, do not use over plain HTTP in production"
        );
    }
    // Build the CAPTCHA provider once, share it across the auth + app
    // state. The factory rejects half-configured upstream providers up
    // front so a missing key surfaces on boot rather than at the first
    // verify.
    let captcha_provider = captcha_module::build_provider(&config.captcha)
        .map_err(|e| anyhow::anyhow!("initialising captcha provider: {e}"))?;
    tracing::info!(
        provider = ?config.captcha.provider,
        apply_to_registration = config.captcha.apply_to_registration,
        apply_to_anonymous_edits = config.captcha.apply_to_anonymous_edits,
        "captcha provider wired",
    );

    let auth_state = AuthState::new(
        storage.clone(),
        hasher,
        session_ttl,
        secure_cookies,
        config.auth.clone(),
    )
    .with_captcha(config.captcha.clone(), Arc::clone(&captcha_provider));
    // Bring up the Tantivy index + indexer worker. Opening the index is
    // synchronous; we run it through `spawn_blocking` so the runtime stays
    // responsive even when the directory is cold. On startup we check the
    // `.last_indexed` marker — if it is missing we trigger a full rebuild
    // from storage so the index never silently lags behind a previous
    // crash.
    let index_path = config.search.index_path.clone();
    let search_index = tokio::task::spawn_blocking({
        let path = index_path.clone();
        move || SearchIndex::open(&path)
    })
    .await
    .context("spawning blocking search-index open")?
    .with_context(|| format!("opening search index at {}", index_path.display()))?;
    let needs_rebuild = !search_index.has_last_indexed_marker();
    // Share the `Arc<SearchIndex>` between the indexer worker (writes) and
    // the searcher handle (reads). Tantivy's `OnCommitWithDelay` reader
    // reload picks up committed writes within the configured window, so the
    // read side stays consistent without us having to plumb anything else.
    let search_index = std::sync::Arc::new(search_index);
    let searcher = Searcher::new(std::sync::Arc::clone(&search_index));
    let indexer_handle = Indexer::new(std::sync::Arc::clone(&search_index))
        .with_commit_interval(Duration::from_millis(config.search.commit_interval_ms))
        .with_commit_batch(config.search.batch_size as usize)
        .spawn();
    if needs_rebuild {
        info!(
            path = %index_path.display(),
            "search .last_indexed marker missing; scheduling full reindex"
        );
        spawn_startup_rebuild(storage.clone(), indexer_handle.clone());
    }

    let mut app_state = thewiki_api::state::AppState::new(storage.clone(), config.auth.clone())
        .with_auth_state(auth_state.clone())
        .with_search(indexer_handle)
        .with_searcher(searcher)
        .with_search_title_boost(config.search.title_boost)
        .with_captcha(config.captcha.clone(), Arc::clone(&captcha_provider));
    let media_backend = thewiki_api::media::build_media_backend(
        &config.storage.backend,
        std::sync::Arc::clone(&app_state.storage),
    )
    .map_err(|e| anyhow::anyhow!("media backend init: {e}"))?;
    app_state = app_state.with_media(config.storage.media.clone(), media_backend);

    // Blocklist (#42): hydrate the in-memory snapshot from storage before
    // we bind the listener so the first request is already gated. A read
    // failure here aborts startup — the operator asked for this protection
    // and we should not silently serve un-blocked traffic.
    let blocklist_state = thewiki_api::blocklist::BlocklistState::empty();
    blocklist_state
        .refresh_from(&storage.ip_blocklist(), &storage.url_blocklist())
        .await
        .context("hydrating blocklist snapshot from storage")?;
    app_state = app_state.with_blocklist(blocklist_state);

    // Build the rate-limit state with the configured backend. The in-memory
    // backend is infallible; the Redis backend (gated behind the `redis`
    // feature) connects up front so a malformed URL surfaces at startup
    // rather than on the first request.
    let rate_limit_state = RateLimitState::connect(
        config.rate_limit.clone(),
        app_state.auth_state.clone().or(Some(auth_state.clone())),
    )
    .await
    .map_err(|e| anyhow::anyhow!("initialising rate limiter: {e}"))?;
    let router = app::build_full_with_rate_limit_state(
        app_state,
        auth_state,
        config.server.serve_frontend,
        rate_limit_state,
        config.graphql.clone(),
        config.security.clone(),
    );

    let listener = tokio::net::TcpListener::bind(&config.server.bind)
        .await
        .with_context(|| format!("binding TCP listener on {}", config.server.bind))?;

    let local_addr = listener
        .local_addr()
        .with_context(|| "reading listener local address")?;
    info!(bind = %local_addr, "thewiki listening");

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .context("axum server terminated with an error")?;

    Ok(())
}

async fn prune_expired_audit_log(
    storage: &SqliteStorage,
    retention_days: u32,
) -> Result<u64, thewiki_storage::StorageError> {
    let cutoff = OffsetDateTime::now_utc() - TimeDuration::days(i64::from(retention_days));
    storage.audit_log().prune_before(cutoff).await
}

fn spawn_audit_log_pruner(storage: SqliteStorage, retention_days: u32) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60 * 60));
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        interval.tick().await;

        loop {
            interval.tick().await;
            match prune_expired_audit_log(&storage, retention_days).await {
                Ok(0) => {}
                Ok(rows) => info!(rows, "pruned expired audit log rows"),
                Err(err) => warn!(error = %err, "failed to prune expired audit log rows"),
            }
        }
    });
}

/// Run a `config` subcommand. Returns the process exit code rather than
/// bubbling errors through anyhow so a validation failure surfaces as a clean
/// non-zero exit (the user-visible behaviour the smoke tests rely on).
fn run_config(cmd: ConfigCommand) -> ExitCode {
    match cmd {
        ConfigCommand::Check { file } => match Config::load(file.as_deref()) {
            Ok(cfg) => match cfg.validate() {
                Ok(()) => {
                    println!("config OK");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    error!(error = %e, "configuration is invalid");
                    eprintln!("config check failed: {e}");
                    ExitCode::FAILURE
                }
            },
            Err(e) => {
                error!(error = %e, "failed to load configuration");
                eprintln!("config check failed: {e}");
                ExitCode::FAILURE
            }
        },
        ConfigCommand::Print { file, json } => match Config::load(file.as_deref()) {
            Ok(cfg) => match render_config(&cfg, json) {
                Ok(out) => {
                    println!("{out}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("failed to serialise config: {e}");
                    ExitCode::FAILURE
                }
            },
            Err(e) => {
                eprintln!("failed to load configuration: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

/// Stream every page's head revision from storage into the indexer. Used by
/// the startup rebuild path when `.last_indexed` is missing — we replay the
/// authoritative state through the same async pipeline as live edits so
/// there is exactly one indexing path to reason about.
fn spawn_startup_rebuild(storage: SqliteStorage, handle: thewiki_search::IndexerHandle) {
    tokio::spawn(async move {
        if let Err(err) = stream_pages_to_indexer(&storage, &handle).await {
            warn!(error = %err, "startup search rebuild encountered an error");
        } else {
            info!("startup search rebuild scheduled all pages");
        }
    });
}

async fn stream_pages_to_indexer(
    storage: &SqliteStorage,
    handle: &thewiki_search::IndexerHandle,
) -> anyhow::Result<u64> {
    // Tell the worker to wipe before we replay — idempotent if the worker
    // beat us to it (e.g. a manual `thewiki reindex` is racing the startup
    // job).
    handle.try_send(thewiki_search::IndexJob::Rebuild);
    let namespaces = storage.namespaces().list().await?;
    let ns_by_id: std::collections::HashMap<_, _> = namespaces
        .iter()
        .map(|ns| (ns.id, ns.slug.as_str().to_string()))
        .collect();
    let mut count: u64 = 0;
    for ns in &namespaces {
        let mut cursor: Option<Cursor> = None;
        loop {
            let slice = storage
                .pages()
                .list_in_namespace(ns.id, cursor.clone(), 200)
                .await?;
            for page in &slice.items {
                let body = match page.current_revision_id {
                    Some(rev_id) => storage
                        .revisions()
                        .get_by_id(rev_id)
                        .await
                        .map(|r| r.body)
                        .unwrap_or_default(),
                    None => String::new(),
                };
                let ns_slug = ns_by_id
                    .get(&page.namespace_id)
                    .cloned()
                    .unwrap_or_default();
                handle.upsert(PageDoc {
                    page_id: page.id,
                    namespace_id: page.namespace_id,
                    namespace_slug: ns_slug,
                    slug: page.slug.clone(),
                    title: page.title.clone(),
                    body,
                    tags: Vec::new(),
                    updated_at: page.updated_at,
                });
                count = count.saturating_add(1);
            }
            cursor = slice.next;
            if cursor.is_none() {
                break;
            }
        }
    }
    Ok(count)
}

/// `thewiki reindex` — open the configured index, wipe it, replay every
/// page's head revision through Tantivy synchronously, and exit.
async fn run_reindex(args: ReindexArgs) -> anyhow::Result<()> {
    let config = Config::load(args.config.as_deref()).context("loading configuration")?;
    config.validate().context("validating configuration")?;

    let storage = SqliteStorage::new(
        &config.database.url,
        SqliteOptions {
            max_connections: config.database.max_connections,
            acquire_timeout: Duration::from_secs(config.database.acquire_timeout_secs),
            foreign_keys: true,
        },
    )
    .await
    .with_context(|| format!("opening storage at {}", config.database.url))?;

    let index_path = config.search.index_path.clone();
    info!(path = %index_path.display(), "rebuilding search index from storage");

    // Collect every page synchronously into a Vec — production wikis are
    // small enough at M0 that this fits in memory; very large deploys can
    // chunk this in a follow-up. We use the same field projection the live
    // upsert path does (see `build_search_doc`).
    let namespaces = storage.namespaces().list().await?;
    let ns_by_id: std::collections::HashMap<_, _> = namespaces
        .iter()
        .map(|ns| (ns.id, ns.slug.as_str().to_string()))
        .collect();

    let mut docs: Vec<PageDoc> = Vec::new();
    for ns in &namespaces {
        let mut cursor: Option<Cursor> = None;
        loop {
            let slice = storage
                .pages()
                .list_in_namespace(ns.id, cursor.clone(), 200)
                .await?;
            for page in &slice.items {
                let body = match page.current_revision_id {
                    Some(rev_id) => storage
                        .revisions()
                        .get_by_id(rev_id)
                        .await
                        .map(|r| r.body)
                        .unwrap_or_default(),
                    None => String::new(),
                };
                let ns_slug = ns_by_id
                    .get(&page.namespace_id)
                    .cloned()
                    .unwrap_or_default();
                docs.push(PageDoc {
                    page_id: page.id,
                    namespace_id: page.namespace_id,
                    namespace_slug: ns_slug,
                    slug: page.slug.clone(),
                    title: page.title.clone(),
                    body,
                    tags: Vec::new(),
                    updated_at: page.updated_at,
                });
            }
            cursor = slice.next;
            if cursor.is_none() {
                break;
            }
        }
    }
    let total = docs.len();
    println!("collected {total} page(s) from storage; rebuilding index ...");

    let rebuild_path = index_path.clone();
    let indexed = tokio::task::spawn_blocking(move || {
        thewiki_search::indexer::rebuild_into(&rebuild_path, docs)
    })
    .await
    .context("spawning blocking reindex worker")??;
    println!(
        "reindex complete: {indexed} document(s) committed to {}",
        index_path.display()
    );
    Ok(())
}

/// `thewiki regen-thumbnails` — walk every media row, re-render the
/// small/medium/large thumbnail variants, and persist them via the
/// configured backend (#33).
///
/// Progress is reported line-by-line to stdout so an operator can watch
/// it stream past. Variant generation is best-effort per row: a failure
/// on one image is logged but doesn't halt the loop.
async fn run_regen_thumbnails(args: RegenThumbnailsArgs) -> anyhow::Result<()> {
    let config = Config::load(args.config.as_deref()).context("loading configuration")?;
    config.validate().context("validating configuration")?;

    let storage = SqliteStorage::new(
        &config.database.url,
        SqliteOptions {
            max_connections: config.database.max_connections,
            acquire_timeout: Duration::from_secs(config.database.acquire_timeout_secs),
            foreign_keys: true,
        },
    )
    .await
    .with_context(|| format!("opening storage at {}", config.database.url))?;

    let backend = media::build_media_backend(
        &config.storage.backend,
        std::sync::Arc::new(storage.clone()),
    )
    .map_err(|e| anyhow::anyhow!("media backend init: {e}"))?;

    let variants_repo = storage.media_variants();
    let mut cursor: Option<thewiki_core::MediaId> = None;
    let mut total = 0u64;
    let mut regenerated = 0u64;
    loop {
        let slice = storage
            .media()
            .list_all(cursor, 100)
            .await
            .context("listing media")?;
        if slice.items.is_empty() {
            break;
        }
        for media_row in &slice.items {
            total += 1;
            let bytes = match backend.get(media_row.id).await {
                Ok(b) => b,
                Err(err) => {
                    eprintln!(
                        "[{}] skipped (fetch failed: {err})",
                        media_row.id.into_uuid()
                    );
                    continue;
                }
            };
            let rendered =
                media::thumbnail::render_in_blocking_pool(media_row.content_type.clone(), bytes)
                    .await;
            if rendered.is_empty() {
                println!(
                    "[{}] no variants generated ({})",
                    media_row.id.into_uuid(),
                    media_row.content_type,
                );
                continue;
            }
            let variant_count = rendered.len();
            media::thumbnail::store_variants(media_row.id, rendered, &variants_repo, &backend)
                .await;
            regenerated += 1;
            println!(
                "[{}] {} variants stored",
                media_row.id.into_uuid(),
                variant_count,
            );
        }
        // The cursor returned by `list_all` is the string form of the last
        // id; for the in-process walk we just grab the last item's id
        // directly so we don't have to re-parse a UUID we already have.
        match slice.items.last() {
            Some(m) if slice.next.is_some() => cursor = Some(m.id),
            _ => break,
        }
    }
    println!("done: regenerated {regenerated} of {total} media row(s)");
    Ok(())
}

fn render_config(cfg: &Config, json: bool) -> anyhow::Result<String> {
    if json {
        serde_json::to_string_pretty(cfg).context("serialising config as JSON")
    } else {
        toml::to_string_pretty(cfg).context("serialising config as TOML")
    }
}
