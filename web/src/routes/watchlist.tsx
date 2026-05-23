//! `/watchlist` — current user's watched pages, with a link to the Atom feed.
//!
//! Authentication is required (the API returns 401 otherwise); we surface the
//! "log in to view" branch directly here so the user doesn't see a generic
//! API error.

import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import {
	ApiError,
	type WatchlistResponse,
	WATCHLIST_ATOM_URL,
	listWatchlist,
} from "../lib/api";

export const Route = createFileRoute("/watchlist")({
	component: WatchlistComponent,
});

function formatTimestamp(iso: string): string {
	try {
		return new Date(iso).toLocaleString();
	} catch {
		return iso;
	}
}

function WatchlistComponent() {
	const query = useQuery<WatchlistResponse, ApiError>({
		queryKey: ["watchlist"],
		queryFn: listWatchlist,
		retry: (failureCount, error) => {
			if (error instanceof ApiError && error.status === 401) {
				return false;
			}
			return failureCount < 1;
		},
	});

	return (
		<main className="mx-auto flex max-w-3xl flex-col gap-6 px-6 py-10">
			<header className="flex flex-wrap items-center justify-between gap-3">
				<h1 className="text-2xl font-semibold tracking-tight">Watchlist</h1>
				<a
					href={WATCHLIST_ATOM_URL}
					className="inline-flex items-center gap-1.5 rounded-md border border-neutral-300 bg-white px-3 py-1.5 text-xs font-medium text-neutral-700 hover:bg-neutral-50"
					title="Subscribe to your watchlist as an Atom feed"
				>
					<FeedIcon />
					Atom feed
				</a>
			</header>

			{query.isPending && (
				<ul className="flex flex-col gap-2">
					{Array.from({ length: 4 }).map((_, idx) => (
						// biome-ignore lint/suspicious/noArrayIndexKey: skeleton placeholders are positional.
						<li key={idx} className="h-10 animate-pulse rounded-md bg-neutral-200" />
					))}
				</ul>
			)}

			{query.isError && query.error instanceof ApiError && query.error.status === 401 && (
				<div className="rounded-md border border-amber-300 bg-amber-50 p-4 text-sm text-amber-800">
					You need to{" "}
					<Link to="/login" className="font-medium underline">
						log in
					</Link>{" "}
					to view your watchlist.
				</div>
			)}

			{query.isError &&
				!(query.error instanceof ApiError && query.error.status === 401) && (
					<div className="rounded-md border border-red-300 bg-red-50 p-3 text-sm text-red-700">
						Failed to load watchlist: {query.error.message}
					</div>
				)}

			{query.isSuccess && query.data.items.length === 0 && (
				<div className="rounded-md border border-dashed border-neutral-300 bg-white p-6 text-center text-sm text-neutral-600">
					You're not watching any pages yet. Open a page and hit the star to
					subscribe.
				</div>
			)}

			{query.isSuccess && query.data.items.length > 0 && (
				<ul className="divide-y divide-neutral-200 overflow-hidden rounded-md border border-neutral-200 bg-white">
					{query.data.items.map((item) => (
						<li key={item.page_id}>
							<Link
								to="/wiki/$namespace/$slug"
								params={{ namespace: item.namespace, slug: item.slug }}
								className="flex items-center justify-between gap-3 px-4 py-3 hover:bg-neutral-50"
							>
								<span className="flex flex-col">
									<span className="font-medium text-neutral-900">{item.title}</span>
									<span
										className="font-mono text-xs text-neutral-500"
										title={`Namespace: ${item.namespace}`}
									>
										{item.namespace}/{item.slug}
									</span>
								</span>
								<span className="text-xs text-neutral-500" title="Watched since">
									{formatTimestamp(item.watched_at)}
								</span>
							</Link>
						</li>
					))}
				</ul>
			)}
		</main>
	);
}

function FeedIcon() {
	return (
		<svg
			aria-hidden
			viewBox="0 0 16 16"
			className="h-3.5 w-3.5 text-orange-600"
			fill="currentColor"
		>
			<title>Atom feed icon</title>
			<path d="M2 2.5A.5.5 0 0 1 2.5 2c6.351 0 11.5 5.149 11.5 11.5a.5.5 0 0 1-1 0C13 7.701 8.299 3 2.5 3a.5.5 0 0 1-.5-.5zm0 5a.5.5 0 0 1 .5-.5A6.5 6.5 0 0 1 9 13.5a.5.5 0 0 1-1 0A5.5 5.5 0 0 0 2.5 8a.5.5 0 0 1-.5-.5zm1.5 4.5a1.5 1.5 0 1 1 0 3 1.5 1.5 0 0 1 0-3z" />
		</svg>
	);
}
