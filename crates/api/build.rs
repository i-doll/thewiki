//! Build-time placeholder bootstrap for the embedded SPA (#16).
//!
//! `rust-embed` reads the asset directory at compile time (release) or at
//! runtime (debug, via the `debug-embed` feature). In both modes, the
//! `#[folder = "..."]` path on the `RustEmbed` derive **must exist** when
//! `cargo build` runs — a fresh checkout without ever having run
//! `pnpm build` in `/web` would fail to compile this crate.
//!
//! That is a terrible first-time-contributor experience. We sidestep it here:
//! if `web/dist/` doesn't already exist, write a tiny placeholder
//! `index.html` so the derive macro has something to enumerate. Real deploys
//! overwrite this with the actual Vite build before the binary is shipped
//! (see the Dockerfile's `web-build` stage).
//!
//! The placeholder explicitly tells the operator they're seeing the
//! development fallback, not a broken production build — if the placeholder
//! ever leaks to production, that's a misconfigured build pipeline and the
//! HTML makes it obvious at a glance.

use std::fs;
use std::path::PathBuf;

fn main() {
    // The path here mirrors the `#[folder = "../../web/dist"]` on
    // `EmbeddedAssets` in `src/static_assets.rs`. Both are relative to
    // `crates/api/`.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dist = manifest_dir.join("..").join("..").join("web").join("dist");

    // Re-run if the dist directory's index.html changes — keeps subsequent
    // `cargo build`s in sync after `pnpm build` writes a fresh bundle. We
    // deliberately don't watch the whole directory: `rust-embed` itself emits
    // `cargo:rerun-if-changed` for every embedded file in release mode.
    println!("cargo:rerun-if-changed=../../web/dist/index.html");

    if dist.join("index.html").exists() {
        return;
    }

    // Placeholder bootstrap: create the directory and a clearly-labelled
    // index.html. This keeps `cargo build` working on a clean checkout where
    // the contributor hasn't run `pnpm build` yet.
    if let Err(e) = fs::create_dir_all(&dist) {
        // Falling back to a warning rather than a hard error: if we can't
        // create the directory, the rust-embed derive will surface its own
        // (clearer) error pointing at the missing folder.
        println!(
            "cargo:warning=could not create placeholder {}: {e}",
            dist.display()
        );
        return;
    }

    let placeholder = "<!doctype html>\n\
<html lang=\"en\">\n\
<head><meta charset=\"utf-8\"><title>thewiki — frontend not built</title></head>\n\
<body>\n\
<h1>thewiki API is running, but the SPA bundle has not been built.</h1>\n\
<p>This page is a placeholder injected by <code>crates/api/build.rs</code>.\n\
Run <code>pnpm install &amp;&amp; pnpm build</code> in <code>/web</code> to produce the real bundle, \
then rebuild the Rust binary.</p>\n\
</body>\n\
</html>\n";

    let index_path = dist.join("index.html");
    if let Err(e) = fs::write(&index_path, placeholder) {
        println!(
            "cargo:warning=could not write placeholder {}: {e}",
            index_path.display()
        );
    }
}
