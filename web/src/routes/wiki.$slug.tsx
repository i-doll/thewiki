import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { useMemo } from "react";
import { ApiError, fetchPage, type PageView } from "../lib/api";
import { renderMarkdown } from "../lib/markdown";

export const Route = createFileRoute("/wiki/$slug")({
	component: PageViewComponent,
});

function formatTimestamp(iso: string): string {
	try {
		const date = new Date(iso);
		return date.toLocaleString();
	} catch {
		return iso;
	}
}

function PageViewComponent() {
	const { slug } = Route.useParams();
	const query = useQuery<PageView, ApiError>({
		queryKey: ["page", slug],
		queryFn: () => fetchPage(slug),
		// Don't waste a round trip retrying a 404 — the "not found" branch is
		// load-bearing UX, not an error.
		retry: (failureCount, error) => {
			if (error instanceof ApiError && error.status === 404) {
				return false;
			}
			return failureCount < 1;
		},
	});

	const renderedHtml = useMemo(() => {
		if (!query.data) {
			return "";
		}
		return renderMarkdown(query.data.content);
	}, [query.data]);

	if (query.isPending) {
		return (
			<main className="mx-auto max-w-5xl px-6 py-10">
				<div className="grid grid-cols-1 gap-6 lg:grid-cols-[1fr_16rem]">
					<div className="flex flex-col gap-4">
						<div className="h-8 w-2/3 animate-pulse rounded bg-neutral-200" />
						<div className="h-4 w-full animate-pulse rounded bg-neutral-200" />
						<div className="h-4 w-11/12 animate-pulse rounded bg-neutral-200" />
						<div className="h-4 w-10/12 animate-pulse rounded bg-neutral-200" />
					</div>
					<aside className="h-48 animate-pulse rounded-md bg-neutral-200" />
				</div>
			</main>
		);
	}

	if (query.isError) {
		if (query.error instanceof ApiError && query.error.status === 404) {
			return <PageNotFound slug={slug} />;
		}
		return (
			<main className="mx-auto max-w-3xl px-6 py-10">
				<div className="rounded-md border border-red-300 bg-red-50 p-4 text-sm text-red-700">
					Failed to load page: {query.error.message}
				</div>
			</main>
		);
	}

	const page = query.data;

	return (
		<main className="mx-auto max-w-5xl px-6 py-10">
			<div className="grid grid-cols-1 gap-8 lg:grid-cols-[1fr_16rem]">
				<article>
					<header className="mb-6 border-b border-neutral-200 pb-4">
						<p className="font-mono text-xs text-neutral-500">
							{page.namespace_slug} / {page.slug}
						</p>
						<h1 className="mt-1 text-3xl font-semibold tracking-tight">{page.title}</h1>
					</header>
					{page.content.trim().length === 0 ? (
						<p className="text-sm italic text-neutral-500">This page is empty.</p>
					) : (
						<div
							className="prose max-w-none"
							// biome-ignore lint/security/noDangerouslySetInnerHtml: content is sanitised through DOMPurify in renderMarkdown. TODO: drop the client-side render once the API ships pre-rendered HTML.
							dangerouslySetInnerHTML={{ __html: renderedHtml }}
						/>
					)}
					<div className="mt-8 flex gap-3 border-t border-neutral-200 pt-4">
						<Link
							to="/wiki/$slug/edit"
							params={{ slug: page.slug }}
							className="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-neutral-800"
						>
							Edit
						</Link>
						<Link
							to="/wiki"
							className="rounded-md border border-neutral-300 bg-white px-3 py-1.5 text-sm font-medium text-neutral-800 hover:bg-neutral-100"
						>
							All pages
						</Link>
					</div>
				</article>

				<aside className="flex flex-col gap-4 rounded-md border border-neutral-200 bg-white p-4 text-sm">
					<div>
						<h2 className="text-xs font-medium uppercase tracking-wide text-neutral-500">
							Last edited
						</h2>
						<p className="mt-1 text-neutral-800">{formatTimestamp(page.updated_at)}</p>
					</div>
					<div>
						<h2 className="text-xs font-medium uppercase tracking-wide text-neutral-500">
							Last editor
						</h2>
						{/* TODO(#19): join the user row in the API response and show the username here. */}
						<p className="mt-1 text-neutral-800">—</p>
					</div>
					<div>
						<h2 className="text-xs font-medium uppercase tracking-wide text-neutral-500">
							Revisions
						</h2>
						{/* TODO(#19): expose revision count via the API. */}
						<p className="mt-1 text-neutral-800">—</p>
					</div>
					<div>
						{/*
						 * TODO(#19): the history view doesn't exist yet — issue #19 wires
						 * up `/wiki/$slug/history`. The link points there so the wiring
						 * is in place when the route lands.
						 */}
						<a
							href={`/wiki/${encodeURIComponent(page.slug)}/history`}
							className="text-xs font-medium text-neutral-700 underline hover:text-neutral-900"
						>
							View history →
						</a>
					</div>
				</aside>
			</div>
		</main>
	);
}

function PageNotFound({ slug }: { slug: string }) {
	return (
		<main className="mx-auto max-w-2xl px-6 py-16 text-center">
			<p className="font-mono text-xs uppercase tracking-wide text-neutral-500">404</p>
			<h1 className="mt-2 text-3xl font-semibold tracking-tight">Page not found</h1>
			<p className="mt-2 text-neutral-600">
				No page with the slug <code className="rounded bg-neutral-200 px-1 font-mono">{slug}</code>{" "}
				exists yet.
			</p>
			<div className="mt-6 flex justify-center gap-3">
				<Link
					to="/wiki/$slug/edit"
					params={{ slug }}
					search={{ new: 1 }}
					className="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-neutral-800"
				>
					Create this page
				</Link>
				<Link
					to="/wiki"
					className="rounded-md border border-neutral-300 bg-white px-3 py-1.5 text-sm font-medium text-neutral-800 hover:bg-neutral-100"
				>
					All pages
				</Link>
			</div>
		</main>
	);
}
