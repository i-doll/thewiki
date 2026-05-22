//! CodeMirror completion source for `[[wiki-link]]` autocomplete.
//!
//! Activates whenever the text immediately before the caret matches
//! `\[\[[^\]\n]*$` — i.e. the user has typed `[[` and may have started
//! filling in a page name, but hasn't yet closed it or hit a newline.
//!
//! Results are sourced via `searchPages` so the same TanStack Query cache
//! used by the Tiptap dropdown deduplicates lookups. The completion source
//! reads the cache *opportunistically* via the `QueryClient`: on a cache miss
//! it `fetchQuery`s, which both populates the cache for the React side and
//! returns the data here.

import type { Completion, CompletionContext, CompletionResult } from "@codemirror/autocomplete";
import type { QueryClient } from "@tanstack/react-query";
import { type SearchResponse, searchPages, searchQueryKey } from "../../lib/search";

const TRIGGER_RE = /\[\[([^\]\n]*)$/;
const MAX_RESULTS = 5;

export interface WikiLinkCompletionOptions {
	queryClient: QueryClient;
	namespace?: string;
}

/**
 * Build a CodeMirror `CompletionSource` that exposes wiki-link suggestions.
 *
 * The source replaces the entire `[[query` prefix with `[[Target]]` on
 * acceptance, so the user ends up with a balanced wiki-link regardless of
 * whether they typed the closing brackets.
 *
 * The "Create [[query]]" fallback is appended as a low-`boost` completion so
 * it ranks below real hits, but is always available when the user has typed
 * at least one character — preserving the redlink path.
 */
export function buildWikiLinkCompletion(
	options: WikiLinkCompletionOptions,
): (context: CompletionContext) => Promise<CompletionResult | null> {
	const { queryClient, namespace } = options;

	return async (context: CompletionContext) => {
		const match = context.matchBefore(TRIGGER_RE);
		if (!match) {
			return null;
		}
		// Only fire implicitly once the user has typed at least one query
		// character, otherwise CodeMirror would pop the dropdown on the bare
		// `[[` keystroke with no useful options. `explicit` (Ctrl+Space) still
		// triggers it on empty input.
		const query = match.text.slice(2);
		if (!context.explicit && query.length === 0) {
			return null;
		}

		// Look up via the shared TanStack Query cache. `fetchQuery` returns the
		// cached value if it's still fresh and otherwise calls the network. We
		// catch errors so a 404 (search route not yet deployed) degrades to the
		// "create redlink" CTA rather than swallowing the dropdown.
		let response: SearchResponse;
		try {
			response = await queryClient.fetchQuery({
				queryKey: searchQueryKey(query, namespace),
				queryFn: ({ signal }) =>
					searchPages(query, {
						limit: MAX_RESULTS,
						...(namespace !== undefined ? { namespace } : {}),
						signal,
					}),
				staleTime: 30_000,
				retry: false,
			});
		} catch {
			response = { items: [], total_estimate: 0, next_cursor: null };
		}

		const completions: Completion[] = response.items.slice(0, MAX_RESULTS).map((hit) => ({
			label: hit.title,
			displayLabel: hit.title,
			detail: hit.namespace_slug,
			type: "text",
			apply: `[[${hit.title}]]`,
			// Hits rank above the create-redlink CTA by default. The library
			// further re-scores by string-match quality.
			boost: 1,
		}));

		const trimmedQuery = query.trim();
		if (trimmedQuery.length > 0) {
			completions.push({
				label: trimmedQuery,
				displayLabel: `Create [[${trimmedQuery}]]`,
				detail: "redlink",
				type: "text",
				apply: `[[${trimmedQuery}]]`,
				boost: -99,
			});
		}

		if (completions.length === 0) {
			return null;
		}

		return {
			from: match.from,
			to: match.to,
			options: completions,
			// Stay open as long as the user is still inside the bracket pair.
			// Without this CodeMirror tears down the dropdown after every
			// keystroke and refetches even for cached prefixes.
			validFor: /^\[\[[^\]\n]*$/,
			// Library-side filtering matches against `label` (the bare title),
			// but our completions show `displayLabel` — disable filtering so
			// the server-side ranking from Tantivy wins. The user sees exactly
			// what the API returned.
			filter: false,
		};
	};
}
