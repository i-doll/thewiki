# ADR-0001: Markdown renderer

- Status: Accepted
- Date: 2026-05-21
- Decision-makers: @i-doll

## Context

thewiki ships Markdown as its only content format in v1, with additional
formats (AsciiDoc, MediaWiki wikitext, reStructuredText) deferred behind the
`Renderer` trait defined in the `core` crate. The `render` crate will implement
that trait for Markdown and is about to be built out, so the underlying library
needs to be chosen first.

Constraints that drive the decision:

- **Single static binary** is a core product goal. Every dependency, and the
  size of its compiled artifact, matters.
- **GitHub Flavored Markdown** is the de facto baseline users expect: tables,
  task lists, strikethrough, autolinks, footnotes.
- **Wikilinks (`[[Page]]`, `[[Page|Label]]`)** ship in M1. The renderer must
  give us a clean way to handle that syntax — ideally built in, otherwise via
  an extension hook we can drive from `render`.
- **HTML sanitisation is handled by `ammonia`** in a separate pass, so the
  renderer's own sanitiser is not load-bearing. Raw-HTML pass-through is
  acceptable as long as we can post-process the output.
- **The `Renderer` trait is sync** (`fn render(&self, source: &str) -> Html`).
  Async support is not a requirement.
- **Maintenance health** — thewiki is a long-lived project, so we want an
  upstream that is actively maintained and broadly used.

The two viable Rust libraries are `pulldown-cmark` and `comrak`. Both target
CommonMark, both support GFM, both are actively maintained as of May 2026.

## Decision

**We will use `pulldown-cmark`** (version 0.13.x at time of writing) behind the
`render` crate's implementation of the `Renderer` trait.

Rationale, ordered by weight against thewiki's constraints:

1. **Binary size and dependency footprint.** `pulldown-cmark` has three
   non-optional runtime dependencies (`bitflags`, `memchr`, `unicase`) and a
   pull-parser architecture that allocates a bare minimum. `comrak` builds a
   full AST in a `typed-arena` and pulls in `caseless`, `entities`,
   `finl_unicode`, `jetscii`, `phf`, `rustc-hash`, `smallvec`, and
   `typed-arena`. For a project whose top-line promise is "single static
   binary," the smaller transitive tree wins.
2. **Wikilinks are first-class.** Since 0.13.0 (February 2024), `pulldown-cmark`
   ships `Options::ENABLE_WIKILINKS` with full Obsidian-style syntax: `[[Page]]`,
   `[[Page/Sub]]`, `[[Page|Label]]`, `![[image.png|alt]]`, and wikilinks as
   autolink replacements. We get M1's wikilink story essentially for free —
   we resolve the destination string in our event handler.
3. **The event-stream API plugs cleanly behind a `Renderer` trait.** The pull
   parser exposes `Iterator<Item = Event>`, which we can intercept, rewrite,
   or replace before handing off to `html::push_html`. Custom rendering for
   wikilinks, attachment links, search-indexable headings, and anchor IDs is
   straightforward without touching the parser.
4. **Performance.** Public benchmarks consistently put `pulldown-cmark` at
   roughly 5x the throughput of `comrak` because it never materialises an AST.
   thewiki renders Markdown on every page view (with a cache layer in front),
   so latency at the renderer matters.
5. **Adoption signal.** `pulldown-cmark` has ~1,122 reverse dependencies on
   crates.io and ~96M downloads; `comrak` has ~230 reverse dependencies and
   ~5.1M downloads. The Rust ecosystem has converged on `pulldown-cmark` for
   embedded use (mdBook, `rustdoc`, Zola, Cobalt, Hugo-style static site
   generators in Rust), which means more battle-testing and a larger pool of
   reference code to learn from.

`ammonia` handles sanitisation downstream in both cases, so `comrak`'s
built-in HTML scrubbing is not a deciding factor.

## Alternatives considered

### pulldown-cmark

**Pros**

- Minimal dependency tree; smallest contribution to final binary size.
- Pull-parser/event-stream API is a natural fit behind a `Renderer` trait and
  for intercepting events (wikilinks, anchor IDs, attachment links).
- Native `ENABLE_WIKILINKS` with Obsidian-style syntax, including piped labels
  and embedded images.
- Granular GFM flags: `ENABLE_TABLES`, `ENABLE_FOOTNOTES`, `ENABLE_STRIKETHROUGH`,
  `ENABLE_TASKLISTS`, plus `ENABLE_GFM` for the rest.
- Roughly 5x faster than `comrak` on typical documents in published
  benchmarks.
- MIT-licensed; MSRV 1.71.1.
- Released 0.13.4 on 2026-05-20; project moved to its own GitHub org
  (`pulldown-cmark/pulldown-cmark`) and has active maintenance, ~2,571 stars,
  steady release cadence.
- The dominant choice in the Rust ecosystem (`rustdoc`, `mdBook`, Zola, etc.).

**Cons**

- No built-in HTML sanitisation. Not actually a con for us — `ammonia` is
  applied separately.
- Adding fundamentally new block/inline syntax requires forking or layering a
  pre-parse stage; we can't register custom parsers in-process. We do not
  need this for v1 or M1 — wikilinks are built in and other extensions on the
  roadmap (templates, transclusion) live above the renderer.
- Two competing footnote dialects (`ENABLE_FOOTNOTES` vs `ENABLE_OLD_FOOTNOTES`)
  require a one-time choice; we will pick the GFM-compatible one.

**Fit**: Strong. Hits every hard constraint and the soft ones too.

### comrak

**Pros**

- Largest extension surface of any Rust Markdown library: tables, strikethrough,
  autolinks, tasklists, footnotes, description lists, math, emoji shortcodes,
  wikilinks (`wikilinks_title_after_pipe`, `wikilinks_title_before_pipe`),
  underline, spoiler, greentext, GFM alerts, multiline blockquotes, header
  IDs, front matter, CJK-friendly emphasis.
- Full AST in an arena — easier to do node-level mutation, multi-pass
  transforms, or round-trip back to Markdown.
- 100% CommonMark 0.31.2 + GFM spec conformance (passes all 670 GFM tests).
- Production users at scale: crates.io, docs.rs, GitLab, Deno, Lockbook, a
  Reddit fork.
- Built-in HTML scrubbing if you want defence in depth.

**Cons**

- Significantly larger transitive dependency tree (8+ non-optional runtime
  deps vs 3), which inflates the static binary.
- Maintainer themselves states comrak "is not and will not be the fastest"
  because of the AST construction step.
- Wikilinks need pipe-direction config but are otherwise comparable to
  pulldown-cmark's built-in support — no decisive advantage.
- MSRV 1.85 is much newer than pulldown-cmark's 1.71.1; mildly tightens the
  toolchain floor for contributors.
- AST-mutation extensibility is powerful but solves a problem we don't have
  in v1/M1.

**Fit**: Workable, but every advantage it has over pulldown-cmark
(richer extensions, AST mutation, built-in sanitiser) is either unnecessary
given our `ammonia` pass and `Renderer` trait, or actively contradicts the
single-binary goal.

## Consequences

**Positive**

- Smaller release binary; less to audit and update.
- Faster per-render latency, useful for uncached page renders and rebuilds.
- Wikilinks work out of the box with a thin event-stream handler in `render`.
- Joining the largest Rust user base for a Markdown library — easier to find
  patches, examples, and contributors.

**Negative**

- If a future content type needs a syntax extension that lives inside the
  Markdown grammar itself (not as a pre/post pass), we will need to either
  fork `pulldown-cmark`, layer a custom preprocessor, or revisit this ADR.
  This is unlikely for the v1/M1 roadmap.
- We rely on the GFM-flavoured footnote dialect rather than the CommonMark-HS
  alternative; documentation must call this out.

**Neutral**

- `ammonia` remains the sanitisation boundary regardless of renderer choice.
- The `Renderer` trait abstracts the library, so a future switch to `comrak`
  or another implementation is still possible at the cost of one crate rewrite,
  not a project-wide migration.

## References

- pulldown-cmark on crates.io: <https://crates.io/crates/pulldown-cmark> (0.13.4, 2026-05-20)
- pulldown-cmark repository: <https://github.com/pulldown-cmark/pulldown-cmark>
- pulldown-cmark `Options` flags: <https://docs.rs/pulldown-cmark/latest/pulldown_cmark/struct.Options.html>
- pulldown-cmark wikilinks spec: <https://pulldown-cmark.github.io/pulldown-cmark/specs/wikilinks.html>
- comrak on crates.io: <https://crates.io/crates/comrak> (0.52.0, 2026-04-04)
- comrak repository: <https://github.com/kivikakk/comrak>
- comrak `Extension` options: <https://docs.rs/comrak/latest/comrak/options/struct.Extension.html>
- 1Password markdown benchmarks: <https://github.com/1Password/markdown-benchmarks>
- ammonia: <https://crates.io/crates/ammonia>
