//! Full search results page (`/search?q=…`).
//!
//! Walks the search endpoint with `useInfiniteQuery` so pagination is wired
//! ready for the day the backend starts emitting non-null `next_cursor`
//! tokens. Today every response carries `null`, so the "Load more" button
//! is hidden and the page renders a single batch.

import { useInfiniteQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { sanitiseSnippet } from "../components/SearchBox";
import { type SearchResponse, searchPages } from "../lib/search";

const PAGE_SIZE = 25;

interface SearchRouteSearch {
	q?: string;
	namespace?: string;
}

export const Route = createFileRoute("/search")({
	component: SearchResultsComponent,
	validateSearch: (search: Record<string, unknown>): SearchRouteSearch => {
		const q = typeof search.q === "string" ? search.q : undefined;
		const namespace = typeof search.namespace === "string" ? search.namespace : undefined;
		return {
			...(q !== undefined ? { q } : {}),
			...(namespace !== undefined ? { namespace } : {}),
		};
	},
});

function formatTimestamp(iso: string | null): string {
	if (iso === null) {
		return "";
	}
	try {
		return new Date(iso).toLocaleString();
	} catch {
		return iso;
	}
}

function SearchResultsComponent() {
	const { q, namespace } = Route.useSearch();
	const trimmed = (q ?? "").trim();

	const infinite = useInfiniteQuery<SearchResponse>({
		queryKey: ["search", "page", trimmed, namespace ?? null],
		queryFn: ({ pageParam, signal }) =>
			searchPages(trimmed, {
				limit: PAGE_SIZE,
				...(namespace !== undefined ? { namespace } : {}),
				cursor: typeof pageParam === "string" ? pageParam : null,
				signal,
			}),
		initialPageParam: null,
		getNextPageParam: (last) => last.next_cursor ?? null,
		enabled: trimmed.length > 0,
		retry: false,
	});

	const hits = infinite.data?.pages.flatMap((page) => page.items) ?? [];
	const totalEstimate = infinite.data?.pages[0]?.total_estimate ?? 0;

	if (trimmed.length === 0) {
		return (
			<main className="mx-auto flex max-w-3xl flex-col gap-6 px-6 py-10">
				<header>
					<h1 className="text-2xl font-semibold tracking-tight">Search</h1>
				</header>
				<p className="rounded-md border border-dashed border-neutral-300 bg-white p-6 text-center text-sm text-neutral-600">
					Type a query into the search box above to begin.
				</p>
			</main>
		);
	}

	return (
		<main className="mx-auto flex max-w-3xl flex-col gap-6 px-6 py-10">
			<header className="flex flex-col gap-1">
				<h1 className="text-2xl font-semibold tracking-tight">
					Search: <span className="font-mono">{trimmed}</span>
				</h1>
				{infinite.isSuccess && (
					<p className="text-sm text-neutral-600">
						{totalEstimate === 0
							? "No results."
							: `${hits.length} result${hits.length === 1 ? "" : "s"}${
									totalEstimate > hits.length ? ` (≈${totalEstimate} matches)` : ""
								}`}
					</p>
				)}
			</header>

			{infinite.isPending && (
				<ul className="flex flex-col gap-3">
					{Array.from({ length: 5 }).map((_, idx) => (
						// biome-ignore lint/suspicious/noArrayIndexKey: positional skeleton placeholders.
						<li key={idx} className="h-16 animate-pulse rounded-md bg-neutral-200" />
					))}
				</ul>
			)}

			{infinite.isError && (
				<div className="rounded-md border border-red-300 bg-red-50 p-3 text-sm text-red-700">
					Search failed:{" "}
					{infinite.error instanceof Error ? infinite.error.message : "unknown error"}
				</div>
			)}

			{infinite.isSuccess && hits.length === 0 && (
				<p className="rounded-md border border-dashed border-neutral-300 bg-white p-6 text-center text-sm text-neutral-600">
					No pages match <span className="font-mono">{trimmed}</span>.
				</p>
			)}

			{hits.length > 0 && (
				<ul className="flex flex-col gap-3">
					{hits.map((hit) => (
						<li
							key={hit.page_id}
							className="rounded-md border border-neutral-200 bg-white p-4 hover:border-neutral-300"
						>
							<div className="flex items-center justify-between gap-3">
								<Link
									to="/wiki/$namespace/$slug"
									params={{ namespace: hit.namespace_slug, slug: hit.slug }}
									className="text-base font-semibold text-neutral-900 hover:underline"
								>
									{hit.title}
								</Link>
								<span
									className="rounded bg-neutral-100 px-1.5 py-0.5 font-mono text-xs text-neutral-600"
									title={`Namespace: ${hit.namespace_slug}`}
								>
									{hit.namespace_slug}
								</span>
							</div>
							{hit.snippet.length > 0 && (
								<p
									className="mt-1 text-sm text-neutral-700 [&_mark]:bg-yellow-100 [&_mark]:text-neutral-900"
									// biome-ignore lint/security/noDangerouslySetInnerHtml: sanitised via DOMPurify in sanitiseSnippet.
									dangerouslySetInnerHTML={{
										__html: sanitiseSnippet(hit.snippet),
									}}
								/>
							)}
							{hit.updated_at !== null && (
								<p className="mt-2 text-xs text-neutral-500">
									Edited {formatTimestamp(hit.updated_at)}
								</p>
							)}
						</li>
					))}
				</ul>
			)}

			{infinite.hasNextPage && (
				<button
					type="button"
					onClick={() => infinite.fetchNextPage()}
					disabled={infinite.isFetchingNextPage}
					className="self-center rounded-md border border-neutral-300 bg-white px-4 py-2 text-sm font-medium text-neutral-800 hover:bg-neutral-100 disabled:opacity-60"
				>
					{infinite.isFetchingNextPage ? "Loading…" : "Load more"}
				</button>
			)}
		</main>
	);
}
