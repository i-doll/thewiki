# ADR-0002: Template syntax

- Status: Accepted
- Date: 2026-05-23
- Decision-makers: @i-doll

## Context

thewiki ships a `Renderer` trait whose Markdown implementation
([ADR-0001](./0001-markdown-renderer.md)) is event-stream based. M2 introduces
**templates**: reusable wiki snippets stored as pages in a dedicated
`Template` namespace, then expanded into other pages via a transclusion
syntax. This ADR picks that syntax. Implementation lands in issue #45.

Forces that constrain the choice:

- **Migration story.** A meaningful slice of the audience for thewiki is
  operators currently running MediaWiki who want a simpler stack. Anything
  that requires rewriting every `{{Infobox|...}}` to a different syntax is a
  real adoption cost. We do not promise *bug-for-bug* MediaWiki compatibility,
  but the surface syntax should be recognisable enough that the bulk of
  authored templates port over with minimal mechanical changes.
- **Author familiarity.** Even users who never touched MediaWiki tend to
  encounter `{{Template|arg}}` syntax through Wikipedia, Fandom, etc. There
  is a large pool of mental models we can match instead of teaching a new one.
- **Parser tractability.** Templates run as a **pre-pass over the page source
  before Markdown parsing** — by the time `pulldown-cmark` sees the text,
  every `{{...}}` is gone. The pre-pass must be: fast (it runs on every
  uncached render), robust against malformed input (a wiki edit must not
  crash the renderer), and bounded (no infinite recursion, no exponential
  blow-up).
- **Markdown coexistence.** Markdown uses `{` and `}` in fenced-attribute and
  link-attribute syntax in some extension dialects, but pulldown-cmark with
  our enabled flags does not assign meaning to bare `{{...}}` inside body
  text. Templates can safely own that delimiter.
- **Server-side scripting is out of scope** (per the #45 ticket). Templates
  are text substitution with named/positional parameter passing. No Lua, no
  conditional logic beyond what falls out of "use the param if given, default
  otherwise", no arbitrary computation. This rules out the most complex
  failure modes of MediaWiki templates and makes the parser comfortably
  small.

## Decision

**Templates use MediaWiki-compatible surface syntax**:

```
{{Name|positional1|positional2|key=value|key2=value2}}
```

with the following concrete semantics for v1:

1. **Reference form** — `{{Name}}`, `{{Name|...}}`. `Name` is the page title
   inside the `Template` namespace. Slug normalisation matches the rest of
   the wiki (case-insensitive, spaces → underscores).
2. **Namespace addressing** — `{{ns:Foo|...}}` resolves to page `Foo` in
   namespace `ns`. Bare `{{Foo}}` is shorthand for `{{Template:Foo}}`. This
   matches MediaWiki convention so existing templates port directly.
3. **Arguments** — `|`-separated. An argument is positional if it contains no
   un-escaped `=`, named otherwise. Positional arguments are indexed from
   `1` (per MediaWiki convention). Named arguments use the literal name from
   the source.
4. **Parameter access inside template bodies** — `{{{name}}}` for a named or
   positional reference; `{{{name|default}}}` for a reference with a default
   value. Triple-brace is parsed only inside `Template:` pages — on regular
   pages it is rendered as literal text.
5. **Whitespace handling** — argument names and values are trimmed of leading
   and trailing whitespace, matching MediaWiki. To preserve whitespace,
   authors use HTML entities or place content inside fenced code.
6. **Escaping** — a literal `|` or `}}` inside an argument is written with
   HTML entities (`&#124;`, `&#125;&#125;`) or wrapped in a `<nowiki>`-like
   construct (deferred to a follow-up; v1 documents the entity approach).
7. **Recursion limit** — hard cap of **20** expansion levels, configurable
   via the runtime config under `[render.template] max_recursion_depth = 20`.
   Exceeding the limit emits a render-time error pinned to the originating
   call site.
8. **Self-reference and cycles** — a template that includes itself, directly
   or via a cycle, is detected by per-render stack tracking. The cycle is
   broken at the first re-entry and a render error is emitted.
9. **Performance budget** — compiled templates are cached in-process by
   `(template_id, current_revision_id)`. Cache invalidation is automatic
   when the template revision changes. No template body is re-parsed inside
   a single page render — the cache returns the parsed token stream.
10. **Errors** — every failure mode (missing template, recursion limit,
    cycle, malformed source) emits an inline diagnostic with the originating
    line/column from the user-visible page, surfaced to the editor.

Templates are **always evaluated before Markdown parsing**, never inside it.
This means a template can emit raw Markdown (including link syntax, headings,
code fences) and that output participates in Markdown parsing exactly as if
the author had typed it inline. It does **not** mean a template can emit a
malformed half-Markdown construct that gets rescued by the Markdown parser
across the seam — the template author is responsible for emitting a valid
fragment.

## Alternatives considered

### Option A — MediaWiki-compatible (chosen)

**Pros**

- Recognised on sight by anyone who has edited a wiki of consequence in the
  last 20 years.
- A large amount of public template source (Wikipedia infoboxes, navboxes,
  citation templates) is shaped like this; even when our reduced semantics
  reject the more advanced uses, surface migration is a search-and-replace
  job rather than a rewrite.
- The `{{name|arg|key=value}}` parameter style is unambiguous and easy to
  parse: split on top-level `|`, classify each piece by presence of `=`.
- Existing tooling (e.g. mwparserfromhell-style Python tools, Rust
  `parse_wiki_text`) can help us cross-check parser output during testing.
- Triple-brace parameter access (`{{{x}}}`) cleanly separates "call a
  template" from "read my own parameter" without inventing new sigils.

**Cons**

- Full MediaWiki template syntax is famously baroque (`#if`, `#switch`,
  parser functions, magic words). We must explicitly carve those out as
  *not implemented* and accept that some imported templates will need a
  rewrite of their logic-heavy bits.
- Trimming whitespace inside arguments is a footgun for first-time users
  expecting it to be preserved — needs documentation.
- The `{{...}}` delimiter is visually heavy compared with alternatives.

**Fit**: Strong. Hits migration, familiarity, and parser tractability.

### Option B — Handlebars-like (`{{> Template}}`, `{{var}}`)

**Pros**

- Battle-tested syntax with several Rust implementations
  (`handlebars-rust`, `gtmpl`, custom).
- Sharper separation between "partial include" (`{{> name}}`) and "variable
  expansion" (`{{var}}`), which makes parser state easier.
- HTML escaping conventions inherit a well-understood model.

**Cons**

- Zero compatibility with the MediaWiki template corpus. Every imported
  template needs to be rewritten by hand.
- Doesn't match user expectations — wiki editors expect `{{Foo|arg}}`, not
  `{{> Foo arg=...}}`.
- Handlebars semantics (helpers, conditionals, iteration) are richer than
  what we want to ship. Picking the syntax invites pressure to also pick
  the semantics, which we explicitly do not want to do in v1.
- Built for JSON contexts and template *files*, not for inline transclusion
  inside an authored document.

**Fit**: Weak. Most of its strengths are in features we are not building,
while it loses the migration story entirely.

### Option C — Fresh design (e.g. `<<Template arg1 key=value>>`)

**Pros**

- Free choice of delimiters with no inherited baggage.
- Could be tuned to be unambiguous against Markdown's syntax in every
  conceivable extension.

**Cons**

- No migration. Every existing wiki template, regardless of source, needs
  manual conversion.
- New users encounter a syntax they have never seen before and that has no
  documentation outside our own.
- We would spend design time deciding on delimiters and escapes — time
  better spent on the engine.

**Fit**: Weak. Solves a problem (delimiter purity) that we do not have,
at the cost of ones (migration, familiarity) that we do.

### Option D — MediaWiki-compatible *with parser functions*

Same as Option A but committing in v1 to a subset of MediaWiki parser
functions: `{{#if}}`, `{{#switch}}`, `{{#ifexpr}}`, etc.

**Pros**

- Even more imported templates work without modification.

**Cons**

- Parser functions are where MediaWiki templates become a programming
  language, with all the performance and correctness pitfalls that implies.
- Each function is a small but real semantic expansion; cumulatively this
  is a large surface to specify and test.
- `#ifexpr` in particular pulls in numeric expression parsing.
- v1 of thewiki is meant to ship; "MediaWiki parity" is an infinite
  backlog.

**Fit**: Future work. Reasonable to layer on top of Option A in a later
ADR once the base engine is stable.

## Consequences

**Positive**

- Operators migrating from MediaWiki can mechanically convert most
  templates: keep the call sites, port the bodies, remove unsupported
  parser functions.
- New users encounter the syntax they have most likely already seen in the
  wild.
- The parser stays small: top-level split, argument classification,
  recursive expansion with a depth counter.
- Caching by `(template_id, revision_id)` is a one-line trick; we get
  fast renders on repeated transclusion of the same template within a
  request.

**Negative**

- Authors will write `{{#if:...}}` and other parser-function calls and
  expect them to work; we must clearly surface "this is not supported"
  rather than silently expanding to nothing.
- Whitespace-trimming behaviour will surprise some authors; documentation
  needs an explicit callout with a worked example.
- Worked examples for nested transclusion need to be documented before
  users discover edge cases the hard way.

**Neutral**

- Adding parser functions later is a strict superset of the v1 syntax;
  authors can adopt them as they ship without rewriting existing
  templates.
- Storing templates as wiki pages in a `Template` namespace reuses all the
  revision-history, search, and protection machinery we already built —
  no separate "template store" subsystem.

## Worked examples

**Simple transclusion** (call site → expanded output):

```text
{{Greeting|Aida}}
```

with `Template:Greeting` body:

```text
Hello, {{{1}}}!
```

renders to:

```text
Hello, Aida!
```

**Named arguments with default**:

```text
{{Welcome|name=Aida|role=Editor}}
```

with `Template:Welcome` body:

```text
Welcome **{{{name}}}** — your role is {{{role|guest}}}.
```

renders to:

```markdown
Welcome **Aida** — your role is Editor.
```

If `role=` is omitted, the default `guest` is used.

**Recursion-limit error**:

```text
{{Loopy}}
```

with `Template:Loopy` body:

```text
{{Loopy}}
```

renders to an inline error block, line/column pinned to the call site:

```text
[template error: recursion limit exceeded (20) at Loopy → Loopy → …]
```

**Missing template**:

```text
{{NoSuchTemplate|arg}}
```

renders to:

```text
[template error: template `Template:NoSuchTemplate` not found]
```

**Cycle**:

```text
A includes B; B includes A.
```

The cycle is broken at the first re-entry and emits the same recursion-limit
style error with the chain printed for diagnosis.

## References

- MediaWiki transclusion syntax — <https://www.mediawiki.org/wiki/Transclusion>
- MediaWiki template parameters — <https://www.mediawiki.org/wiki/Help:Templates>
- mwparserfromhell (Python reference parser) — <https://github.com/earwig/mwparserfromhell>
- `parse_wiki_text` (Rust) — <https://crates.io/crates/parse_wiki_text>
- ADR-0001 (Markdown renderer) — [./0001-markdown-renderer.md](./0001-markdown-renderer.md)
- Implementation issue — <https://github.com/i-doll/thewiki/issues/45>
