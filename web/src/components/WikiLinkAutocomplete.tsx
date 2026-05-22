//! Floating autocomplete dropdown for `[[wiki-link]]` insertion.
//!
//! Used by both editor modes (Tiptap + CodeMirror). The component is purely
//! presentational + keyboard-driven: it doesn't know how the host editor will
//! apply the selection. Callers pass an `onSelect(target)` that receives either
//! a chosen page title (when a hit is picked) or the raw query (when the
//! "Create [[query]]" fallback is taken — the redlink path).
//!
//! Data flow:
//!   - The `query` prop is debounced for 150 ms here so the host editor can
//!     update it on every keystroke without flooding the search endpoint.
//!   - Results come from `searchPages` via TanStack Query; the query key is
//!     shared with the CodeMirror completion source so identical lookups
//!     deduplicate across editor modes.
//!   - Keyboard handling lives on `document` (capture phase) so it works
//!     regardless of which editor instance currently holds DOM focus.

import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import { type SearchHit, searchPages, searchQueryKey } from "../lib/search";

const MAX_RESULTS = 5;
const DEBOUNCE_MS = 150;

export interface WikiLinkAutocompleteProps {
	/** Live (un-debounced) query string captured from the host editor. */
	query: string;
	/** Viewport-relative anchor for the dropdown — typically the caret rect. */
	position: { top: number; left: number };
	/** Called with the chosen target (page title or raw query for redlinks). */
	onSelect: (target: string) => void;
	/** Called when the user dismisses the dropdown (Esc, click-outside). */
	onClose: () => void;
	/** Optional namespace bias passed through to the search endpoint. */
	namespace?: string;
}

/**
 * Returns a value that updates only after `delay` ms of stillness on its
 * input. Used here to keep the search endpoint quiet while the user is still
 * typing into the editor.
 */
function useDebounced<T>(value: T, delay: number): T {
	const [debounced, setDebounced] = useState(value);
	useEffect(() => {
		const handle = window.setTimeout(() => setDebounced(value), delay);
		return () => window.clearTimeout(handle);
	}, [value, delay]);
	return debounced;
}

/**
 * Escape special regex characters in user input so we can build a safe
 * highlight pattern. Mirrors the standard MDN snippet — keep in sync if you
 * adjust the character class.
 */
function escapeRegExp(input: string): string {
	return input.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Split a title into highlighted spans for the matching segments of `query`.
 * Case-insensitive match. Returns the original title as a single span when
 * the query is empty or no match is found — keeping the render branch-free.
 */
function highlightMatch(title: string, query: string): React.ReactNode {
	const trimmed = query.trim();
	if (trimmed.length === 0) {
		return title;
	}
	const pattern = new RegExp(`(${escapeRegExp(trimmed)})`, "ig");
	const parts = title.split(pattern);
	return parts.map((part, idx) => {
		const key = `${idx}-${part}`;
		if (part.toLowerCase() === trimmed.toLowerCase()) {
			return (
				<mark key={key} className="bg-yellow-100 text-neutral-900">
					{part}
				</mark>
			);
		}
		return <span key={key}>{part}</span>;
	});
}

export function WikiLinkAutocomplete({
	query,
	position,
	onSelect,
	onClose,
	namespace,
}: WikiLinkAutocompleteProps) {
	const debouncedQuery = useDebounced(query, DEBOUNCE_MS);
	const containerRef = useRef<HTMLDivElement | null>(null);
	const [selectedIndex, setSelectedIndex] = useState(0);

	const search = useQuery({
		queryKey: searchQueryKey(debouncedQuery, namespace),
		queryFn: ({ signal }) =>
			searchPages(debouncedQuery, {
				limit: MAX_RESULTS,
				...(namespace !== undefined ? { namespace } : {}),
				signal,
			}),
		enabled: debouncedQuery.trim().length > 0,
		// Errors (404 while backend route is in flight, transient failures) fall
		// through to "no results" + the create-redlink CTA. Don't retry — the
		// dropdown should react instantly to the next keystroke instead.
		retry: false,
		staleTime: 30_000,
	});

	const hits: SearchHit[] = useMemo(() => {
		if (!search.data) {
			return [];
		}
		return search.data.items.slice(0, MAX_RESULTS);
	}, [search.data]);

	// The "Create [[query]]" CTA is always available (even while loading) so
	// the user can commit a redlink immediately on Enter without waiting for
	// the network. Total selectable rows = hits + 1.
	const totalRows = hits.length + 1;

	// Keep the selection cursor in bounds whenever the result set changes.
	useEffect(() => {
		setSelectedIndex((current) => (current >= totalRows ? Math.max(0, totalRows - 1) : current));
	}, [totalRows]);

	// Reset selection to the top whenever the (debounced) query changes —
	// otherwise a user who arrowed down then typed more characters would stay
	// on a stale row that may no longer match. `debouncedQuery` isn't read in
	// the effect body, but its change is exactly the signal we react to.
	// biome-ignore lint/correctness/useExhaustiveDependencies: trigger only — not read.
	useEffect(() => {
		setSelectedIndex(0);
	}, [debouncedQuery]);

	// Document-level keyboard handler. We use capture-phase so we win over the
	// host editor's bindings (Tiptap & CodeMirror both register on the editor
	// DOM, which is a deeper ancestor of `document`).
	useEffect(() => {
		const handler = (event: KeyboardEvent) => {
			if (event.key === "ArrowDown") {
				event.preventDefault();
				event.stopPropagation();
				setSelectedIndex((current) => (current + 1) % totalRows);
				return;
			}
			if (event.key === "ArrowUp") {
				event.preventDefault();
				event.stopPropagation();
				setSelectedIndex((current) => (current - 1 + totalRows) % totalRows);
				return;
			}
			if (event.key === "Enter") {
				event.preventDefault();
				event.stopPropagation();
				const trimmedQuery = query.trim();
				if (selectedIndex < hits.length) {
					const hit = hits[selectedIndex];
					if (hit) {
						onSelect(hit.title);
						return;
					}
				}
				if (trimmedQuery.length > 0) {
					onSelect(trimmedQuery);
				} else {
					onClose();
				}
				return;
			}
			if (event.key === "Escape") {
				event.preventDefault();
				event.stopPropagation();
				onClose();
				return;
			}
		};
		document.addEventListener("keydown", handler, true);
		return () => {
			document.removeEventListener("keydown", handler, true);
		};
	}, [hits, onClose, onSelect, query, selectedIndex, totalRows]);

	// Click-outside dismissal. Pointerdown rather than click so we close before
	// the editor sees a focus change that would have closed us anyway.
	useEffect(() => {
		const handler = (event: PointerEvent) => {
			const node = containerRef.current;
			if (!node) {
				return;
			}
			if (event.target instanceof Node && node.contains(event.target)) {
				return;
			}
			onClose();
		};
		document.addEventListener("pointerdown", handler, true);
		return () => {
			document.removeEventListener("pointerdown", handler, true);
		};
	}, [onClose]);

	const trimmedQuery = query.trim();
	const createIndex = hits.length;

	return (
		<div
			ref={containerRef}
			role="listbox"
			aria-label="Wiki link suggestions"
			className="pointer-events-auto fixed z-50 w-72 overflow-hidden rounded-md border border-neutral-200 bg-white text-sm shadow-lg"
			style={{ top: position.top, left: position.left }}
		>
			{search.isLoading && hits.length === 0 && (
				<div className="px-3 py-2 text-xs text-neutral-500">Searching…</div>
			)}

			{hits.length > 0 && (
				<ul className="max-h-64 divide-y divide-neutral-100 overflow-y-auto">
					{hits.map((hit, idx) => {
						const isSelected = idx === selectedIndex;
						return (
							<li key={hit.page_id}>
								<button
									type="button"
									role="option"
									aria-selected={isSelected}
									onMouseEnter={() => setSelectedIndex(idx)}
									onClick={() => onSelect(hit.title)}
									className={`flex w-full items-center justify-between gap-2 px-3 py-2 text-left ${
										isSelected ? "bg-neutral-100" : "bg-white hover:bg-neutral-50"
									}`}
								>
									<span className="truncate font-medium text-neutral-900">
										{highlightMatch(hit.title, trimmedQuery)}
									</span>
									<span className="shrink-0 font-mono text-xs text-neutral-500">
										{hit.namespace_slug}
									</span>
								</button>
							</li>
						);
					})}
				</ul>
			)}

			{trimmedQuery.length > 0 && (
				<button
					type="button"
					role="option"
					aria-selected={selectedIndex === createIndex}
					onMouseEnter={() => setSelectedIndex(createIndex)}
					onClick={() => onSelect(trimmedQuery)}
					className={`flex w-full items-center justify-between gap-2 border-t border-neutral-100 px-3 py-2 text-left ${
						selectedIndex === createIndex ? "bg-neutral-100" : "bg-white hover:bg-neutral-50"
					}`}
				>
					<span className="truncate text-neutral-700">
						Create <span className="font-mono text-neutral-900">[[{trimmedQuery}]]</span>
					</span>
					<span className="shrink-0 text-xs text-neutral-500">redlink</span>
				</button>
			)}

			{trimmedQuery.length === 0 && hits.length === 0 && (
				<div className="px-3 py-2 text-xs text-neutral-500">Type to search pages…</div>
			)}
		</div>
	);
}

export default WikiLinkAutocomplete;
