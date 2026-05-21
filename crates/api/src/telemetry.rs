//! Structured logging setup.
//!
//! The binary calls [`init`] once at startup. Tests don't initialise tracing
//! (or initialise it via their own harness) so this module is intentionally
//! tiny.

use std::sync::OnceLock;

use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Initialises the global tracing subscriber.
///
/// Idempotent: calling it more than once is a no-op. The filter is read from
/// `RUST_LOG` (falling back to `info`). The output format is JSON by default,
/// or human-readable when `THEWIKI_LOG_FORMAT=pretty`.
pub fn init() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(install_subscriber);
}

fn install_subscriber() {
    let env_filter = EnvFilter::builder()
        .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
        .from_env_lossy();

    let pretty = std::env::var("THEWIKI_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("pretty"))
        .unwrap_or(false);

    let registry = tracing_subscriber::registry().with(env_filter);

    if pretty {
        let layer = fmt::layer().with_target(true);
        // `try_init` rather than `init`: if some other code has already
        // installed a global subscriber (e.g. a test harness) we keep theirs
        // rather than panicking.
        let _ = registry.with(layer).try_init();
    } else {
        let layer = fmt::layer()
            .json()
            .with_target(true)
            .with_current_span(true);
        let _ = registry.with(layer).try_init();
    }
}
