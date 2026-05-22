//! Typed wrapper around the search endpoint used by wiki-link autocomplete.
//!
//! The autocomplete in both editor modes funnels through `searchPages` so the
//! TanStack Query cache deduplicates identical lookups regardless of which
//! editor surface issued them. The endpoint shape mirrors `SearchResults` /
//! `SearchHit` in `crates/search/src/results.rs` — see issue #26.
//!
//! Until the search HTTP route lands the request will 404. The autocomplete
//! UI treats any error (including 404) as "no matches", which falls back to
//! the "create redlink" CTA — so the feature degrades gracefully.

import { ApiError } from "./api";

/** One hit returned by `GET /api/v1/search`. Mirrors `SearchHit`. */
export interface SearchHit {
	page_id: string;
	namespace_slug: string;
	slug: string;
	title: string;
	/** HTML snippet with `<mark>…</mark>` highlights. May be empty. */
	snippet: string;
	score: number;
	updated_at: string | null;
}

/** Response body. Mirrors `SearchResults`. */
export interface SearchResponse {
	items: SearchHit[];
	total_estimate: number;
	next_cursor: string | null;
}

export interface SearchOptions {
	/** Cap on returned items. Defaults to 5 — sized for an autocomplete dropdown. */
	limit?: number;
	/** Optional namespace bias. Today the backend treats this as a strict filter. */
	namespace?: string;
	/** Opaque cursor returned by a previous call. Reserved for the next pagination PR. */
	cursor?: string | null;
	/** Abort signal for query cancellation when the caller is unmounted. */
	signal?: AbortSignal;
}

/**
 * Fetch matching pages for the given query.
 *
 * Returns an empty list (rather than throwing) for empty queries so callers
 * don't have to special-case the "nothing typed yet" state.
 */
export async function searchPages(
	query: string,
	options: SearchOptions = {},
): Promise<SearchResponse> {
	const trimmed = query.trim();
	if (trimmed.length === 0) {
		return { items: [], total_estimate: 0, next_cursor: null };
	}

	const params = new URLSearchParams();
	params.set("q", trimmed);
	if (options.limit !== undefined) {
		params.set("limit", String(options.limit));
	}
	if (options.namespace && options.namespace.length > 0) {
		params.set("namespace", options.namespace);
	}
	if (options.cursor) {
		params.set("cursor", options.cursor);
	}

	const res = await fetch(`/api/v1/search?${params.toString()}`, {
		method: "GET",
		signal: options.signal ?? null,
	});
	if (!res.ok) {
		// Surface the status so callers can distinguish "no endpoint" (404 while
		// the backend route is still in flight) from a real failure; both paths
		// degrade to the empty list in the UI today.
		throw new ApiError(res.status, `${res.status} ${res.statusText}`);
	}
	return (await res.json()) as SearchResponse;
}

/**
 * Build a stable TanStack Query key for a search lookup. Centralised so both
 * editor modes hit the same cache entries.
 */
export function searchQueryKey(query: string, namespace?: string): readonly unknown[] {
	return ["search", query.trim(), namespace ?? null] as const;
}
