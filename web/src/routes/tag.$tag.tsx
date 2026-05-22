import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { ApiError, fetchTag, type TagDetailResponse } from "../lib/api";

export const Route = createFileRoute("/tag/$tag")({
	component: TagComponent,
});

function TagComponent() {
	const { tag } = Route.useParams();
	const query = useQuery<TagDetailResponse, ApiError>({
		queryKey: ["tag", tag],
		queryFn: () => fetchTag(tag),
		retry: (failureCount, error) => {
			if (error instanceof ApiError && error.status === 400) {
				return false;
			}
			return failureCount < 1;
		},
	});

	if (query.isPending) {
		return (
			<main className="mx-auto max-w-3xl px-6 py-10">
				<div className="h-8 w-1/2 animate-pulse rounded bg-neutral-200" />
			</main>
		);
	}

	if (query.isError) {
		return (
			<main className="mx-auto max-w-3xl px-6 py-10">
				<div className="rounded-md border border-red-300 bg-red-50 p-4 text-sm text-red-700">
					Failed to load tag: {query.error.message}
				</div>
			</main>
		);
	}

	const { items } = query.data;
	return (
		<main className="mx-auto max-w-3xl px-6 py-10">
			<header className="mb-6 border-b border-neutral-200 pb-4">
				<p className="font-mono text-xs uppercase tracking-wide text-neutral-500">Tag</p>
				<h1 className="mt-1 text-3xl font-semibold tracking-tight">#{query.data.tag}</h1>
			</header>

			{items.length === 0 ? (
				<p className="text-sm italic text-neutral-500">No pages carry this tag yet.</p>
			) : (
				<ul className="flex flex-col gap-2">
					{items.map((item) => (
						<li key={item.page_id} className="rounded-md border border-neutral-200 bg-white p-3">
							<Link
								to="/wiki/$slug"
								params={{ slug: item.slug }}
								className="text-sm font-medium text-neutral-900 hover:underline"
							>
								{item.title}
							</Link>
							<p className="font-mono text-xs text-neutral-500">
								{item.namespace_slug} / {item.slug}
							</p>
						</li>
					))}
				</ul>
			)}
		</main>
	);
}
