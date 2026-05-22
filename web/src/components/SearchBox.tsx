//! Header search box (#27).
//!
//! Mounted in `__root.tsx` so every page carries the field. Behaviour:
//!
//! - Debounced 200 ms while the user types to keep the network quiet.
//! - Below ~3 chars the dropdown is suppressed entirely (the index hit is
//!   wasted work for a 1-2 char prefix at this scale).
//! - Up to 5 top hits show in a floating dropdown anchored to the input.
//! - Enter (when no row is highlighted) or clicking "See all results"
//!   navigates to `/search?q=…`.
//! - Esc / click-outside dismisses the dropdown.
//!
//! Snippets carry HTML highlights (`<mark>…</mark>`) so we pipe them
//! through DOMPurify before `dangerouslySetInnerHTML` — defence-in-depth
//! against any future renderer regression.

import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import DOMPurify from "dompurify";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { searchPages, searchQueryKey } from "../lib/search";

const DEBOUNCE_MS = 200;
const MIN_CHARS = 2;
const DROPDOWN_LIMIT = 5;

/**
 * Returns a value that updates only after `delay` ms of stillness on its
 * input. Same shape as the helper in `WikiLinkAutocomplete` — duplicated
 * locally so this component has no dependency on autocomplete internals.
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
 * Allowlist sanitiser configured to permit only `<mark>` (and the harmless
 * inline formatting Tantivy never emits). Returns a string suitable for
 * `dangerouslySetInnerHTML`.
 */
export function sanitiseSnippet(html: string): string {
	return DOMPurify.sanitize(html, {
		ALLOWED_TAGS: ["mark", "b", "i", "em", "strong"],
		ALLOWED_ATTR: [],
	});
}

interface SearchBoxProps {
	/** Optional initial value (e.g. when mounted on `/search?q=…`). */
	initialQuery?: string;
}

export function SearchBox({ initialQuery = "" }: SearchBoxProps) {
	const navigate = useNavigate();
	const [value, setValue] = useState(initialQuery);
	const [open, setOpen] = useState(false);
	const debouncedValue = useDebounced(value, DEBOUNCE_MS);
	const containerRef = useRef<HTMLDivElement | null>(null);
	const listboxId = useId();

	// Keep the input in sync if the route's initial query changes (e.g. user
	// hits Back from `/search?q=foo` to `/`).
	useEffect(() => {
		setValue(initialQuery);
	}, [initialQuery]);

	const trimmed = debouncedValue.trim();
	const enabled = trimmed.length >= MIN_CHARS;

	const search = useQuery({
		queryKey: searchQueryKey(trimmed),
		queryFn: ({ signal }) =>
			searchPages(trimmed, {
				limit: DROPDOWN_LIMIT,
				signal,
			}),
		enabled,
		retry: false,
		staleTime: 30_000,
	});

	const hits = useMemo(() => {
		if (!search.data) {
			return [];
		}
		return search.data.items.slice(0, DROPDOWN_LIMIT);
	}, [search.data]);

	// Click-outside dismissal.
	useEffect(() => {
		const handler = (event: PointerEvent) => {
			const node = containerRef.current;
			if (!node) {
				return;
			}
			if (event.target instanceof Node && node.contains(event.target)) {
				return;
			}
			setOpen(false);
		};
		document.addEventListener("pointerdown", handler, true);
		return () => {
			document.removeEventListener("pointerdown", handler, true);
		};
	}, []);

	const submit = () => {
		const q = value.trim();
		if (q.length === 0) {
			return;
		}
		setOpen(false);
		navigate({ to: "/search", search: { q } });
	};

	return (
		<div ref={containerRef} className="relative w-64">
			<search>
				<form
					onSubmit={(e) => {
						e.preventDefault();
						submit();
					}}
				>
					<input
						type="search"
						role="combobox"
						value={value}
						placeholder="Search…"
						aria-label="Search the wiki"
						aria-autocomplete="list"
						aria-controls={listboxId}
						aria-expanded={open}
						onChange={(e) => {
							setValue(e.target.value);
							setOpen(true);
						}}
						onFocus={() => setOpen(value.trim().length > 0)}
						onKeyDown={(e) => {
							if (e.key === "Escape") {
								setOpen(false);
							}
						}}
						className="w-full rounded-md border border-neutral-300 bg-white px-3 py-1.5 text-sm text-neutral-800 placeholder:text-neutral-400 focus:border-neutral-500 focus:outline-none"
					/>
				</form>
			</search>

			{open && enabled && (
				<div
					id={listboxId}
					role="listbox"
					aria-label="Search suggestions"
					className="absolute left-0 right-0 top-full z-40 mt-1 overflow-hidden rounded-md border border-neutral-200 bg-white text-sm shadow-lg"
				>
					{search.isLoading && <div className="px-3 py-2 text-xs text-neutral-500">Searching…</div>}
					{search.isError && (
						<div className="px-3 py-2 text-xs text-red-700">Search unavailable.</div>
					)}
					{search.isSuccess && hits.length === 0 && (
						<div className="px-3 py-2 text-xs text-neutral-500">No results.</div>
					)}
					{hits.length > 0 && (
						<ul className="max-h-96 divide-y divide-neutral-100 overflow-y-auto">
							{hits.map((hit) => (
								<li key={hit.page_id}>
									<button
										type="button"
										role="option"
										aria-selected={false}
										onClick={() => {
											setOpen(false);
											// Navigate to the namespace-aware route (#28). The
											// namespace prefix is visible in the dropdown so the
											// user already sees where they're going.
											navigate({
												to: "/wiki/$namespace/$slug",
												params: { namespace: hit.namespace_slug, slug: hit.slug },
											});
										}}
										className="flex w-full flex-col gap-0.5 px-3 py-2 text-left hover:bg-neutral-50"
									>
										<div className="flex items-center justify-between gap-2">
											<span className="truncate font-medium text-neutral-900">{hit.title}</span>
											<span
												className="shrink-0 rounded bg-neutral-100 px-1.5 py-0.5 font-mono text-xs text-neutral-600"
												title={`Namespace: ${hit.namespace_slug}`}
											>
												{hit.namespace_slug}
											</span>
										</div>
										{hit.snippet.length > 0 && (
											<p
												className="line-clamp-2 text-xs text-neutral-600 [&_mark]:bg-yellow-100 [&_mark]:text-neutral-900"
												// biome-ignore lint/security/noDangerouslySetInnerHtml: sanitised via DOMPurify in sanitiseSnippet.
												dangerouslySetInnerHTML={{
													__html: sanitiseSnippet(hit.snippet),
												}}
											/>
										)}
									</button>
								</li>
							))}
						</ul>
					)}
					{hits.length > 0 && (
						<button
							type="button"
							onClick={submit}
							className="block w-full border-t border-neutral-100 bg-neutral-50 px-3 py-2 text-left text-xs font-medium text-neutral-700 hover:bg-neutral-100"
						>
							See all results for &ldquo;{value.trim()}&rdquo; →
						</button>
					)}
				</div>
			)}
		</div>
	);
}

export default SearchBox;
