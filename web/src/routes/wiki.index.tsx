import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { listPages, type PageListResponse } from "../lib/api";

const PAGE_SIZE = 50;

interface WikiIndexSearch {
	cursor?: string;
}

export const Route = createFileRoute("/wiki/")({
	component: WikiIndexComponent,
	validateSearch: (search: Record<string, unknown>): WikiIndexSearch => {
		const cursor = search.cursor;
		return typeof cursor === "string" && cursor.length > 0 ? { cursor } : {};
	},
});

function WikiIndexComponent() {
	const { cursor } = Route.useSearch();
	const cursorKey = cursor ?? null;
	const query = useQuery<PageListResponse>({
		queryKey: ["pages", "list", cursorKey],
		queryFn: () => listPages({ cursor: cursorKey, limit: PAGE_SIZE }),
	});

	return (
		<main className="mx-auto flex max-w-3xl flex-col gap-6 px-6 py-10">
			<header className="flex items-center justify-between">
				<h1 className="text-2xl font-semibold tracking-tight">Pages</h1>
				<Link
					to="/wiki/$slug/edit"
					params={{ slug: "new-page" }}
					search={{ new: 1 }}
					className="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-neutral-800"
				>
					New page
				</Link>
			</header>

			{query.isPending && (
				<ul className="flex flex-col gap-2">
					{Array.from({ length: 6 }).map((_, idx) => (
						// biome-ignore lint/suspicious/noArrayIndexKey: skeleton placeholders are positional.
						<li key={idx} className="h-10 animate-pulse rounded-md bg-neutral-200" />
					))}
				</ul>
			)}

			{query.isError && (
				<div className="rounded-md border border-red-300 bg-red-50 p-3 text-sm text-red-700">
					Failed to load pages: {query.error.message}
				</div>
			)}

			{query.isSuccess && query.data.items.length === 0 && (
				<div className="rounded-md border border-dashed border-neutral-300 bg-white p-6 text-center text-sm text-neutral-600">
					No pages yet. Click <span className="font-medium">New page</span> to create one.
				</div>
			)}

			{query.isSuccess && query.data.items.length > 0 && (
				<ul className="divide-y divide-neutral-200 overflow-hidden rounded-md border border-neutral-200 bg-white">
					{query.data.items.map((item) => (
						<li key={item.id}>
							<Link
								to="/wiki/$namespace/$slug"
								params={{ namespace: item.namespace_slug, slug: item.slug }}
								className="flex items-center justify-between gap-3 px-4 py-3 hover:bg-neutral-50"
							>
								<span className="font-medium text-neutral-900">{item.title}</span>
								<span
									className="rounded bg-neutral-100 px-1.5 py-0.5 font-mono text-xs text-neutral-600"
									title={`Namespace: ${item.namespace_slug}`}
								>
									{item.namespace_slug}/{item.slug}
								</span>
							</Link>
						</li>
					))}
				</ul>
			)}

			{query.isSuccess && query.data.next_cursor && (
				<div className="flex justify-end">
					<Link
						to="/wiki"
						search={{ cursor: query.data.next_cursor }}
						className="rounded-md border border-neutral-300 bg-white px-3 py-1.5 text-sm font-medium text-neutral-800 hover:bg-neutral-100"
					>
						Next page
					</Link>
				</div>
			)}
		</main>
	);
}
