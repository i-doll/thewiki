//! Heading-anchor slugification.
//!
//! Rules:
//!
//! - lowercase,
//! - replace any run of whitespace with a single `-`,
//! - drop characters that are not ASCII alphanumeric or `-`,
//! - collapse repeated `-` and trim leading/trailing `-`,
//! - if the slug is empty after stripping (e.g. heading was only emoji),
//!   fall back to `section`.
//!
//! Duplicate anchors inside the same document are disambiguated by
//! appending `-2`, `-3`, … via [`SlugAllocator`].

use std::collections::HashMap;

/// Slugify a heading title into a stable HTML `id`.
pub(crate) fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for ch in input.chars() {
        let mapped: Option<char> = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if ch.is_whitespace() || ch == '-' || ch == '_' {
            Some('-')
        } else {
            None
        };
        match mapped {
            Some('-') => {
                if !prev_dash && !out.is_empty() {
                    out.push('-');
                    prev_dash = true;
                }
            }
            Some(c) => {
                out.push(c);
                prev_dash = false;
            }
            None => {}
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "section".into()
    } else {
        out
    }
}

/// Hands out unique slugs within a document.
///
/// First occurrence keeps the bare slug; subsequent collisions get `-2`,
/// `-3`, … appended.
#[derive(Debug, Default)]
pub(crate) struct SlugAllocator {
    used: HashMap<String, usize>,
}

impl SlugAllocator {
    pub(crate) fn allocate(&mut self, raw: &str) -> String {
        let base = slugify(raw);
        let count = self.used.entry(base.clone()).or_insert(0);
        *count += 1;
        if *count == 1 {
            base
        } else {
            format!("{base}-{count}")
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World"), "hello-world");
    }

    #[test]
    fn slugify_strips_punctuation() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
    }

    #[test]
    fn slugify_collapses_whitespace() {
        assert_eq!(slugify("a  b\t c"), "a-b-c");
    }

    #[test]
    fn slugify_trims_dashes() {
        assert_eq!(slugify("-leading-"), "leading");
    }

    #[test]
    fn slugify_only_symbols_falls_back() {
        assert_eq!(slugify("!!! ???"), "section");
    }

    #[test]
    fn allocator_disambiguates_collisions() {
        let mut alloc = SlugAllocator::default();
        assert_eq!(alloc.allocate("Hello World"), "hello-world");
        assert_eq!(alloc.allocate("Hello World"), "hello-world-2");
        assert_eq!(alloc.allocate("Hello World"), "hello-world-3");
    }
}
