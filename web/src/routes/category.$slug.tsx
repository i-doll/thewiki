import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { ApiError, type CategoryDetailResponse, fetchCategory } from "../lib/api";

export const Route = createFileRoute("/category/$slug")({
	component: CategoryComponent,
});

function CategoryComponent() {
	const { slug } = Route.useParams();
	const query = useQuery<CategoryDetailResponse, ApiError>({
		queryKey: ["category", slug],
		queryFn: () => fetchCategory(slug),
		retry: (failureCount, error) => {
			if (error instanceof ApiError && error.status === 404) {
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
		if (query.error instanceof ApiError && query.error.status === 404) {
			return (
				<main className="mx-auto max-w-2xl px-6 py-16 text-center">
					<p className="font-mono text-xs uppercase tracking-wide text-neutral-500">404</p>
					<h1 className="mt-2 text-3xl font-semibold tracking-tight">Category not found</h1>
					<p className="mt-2 text-neutral-600">
						No category with the slug{" "}
						<code className="rounded bg-neutral-200 px-1 font-mono">{slug}</code> exists.
					</p>
					<Link
						to="/wiki"
						className="mt-6 inline-block rounded-md border border-neutral-300 bg-white px-3 py-1.5 text-sm font-medium text-neutral-800 hover:bg-neutral-100"
					>
						All pages
					</Link>
				</main>
			);
		}
		return (
			<main className="mx-auto max-w-3xl px-6 py-10">
				<div className="rounded-md border border-red-300 bg-red-50 p-4 text-sm text-red-700">
					Failed to load category: {query.error.message}
				</div>
			</main>
		);
	}

	const { category, items } = query.data;
	return (
		<main className="mx-auto max-w-3xl px-6 py-10">
			<header className="mb-6 border-b border-neutral-200 pb-4">
				<p className="font-mono text-xs uppercase tracking-wide text-neutral-500">Category</p>
				<h1 className="mt-1 text-3xl font-semibold tracking-tight">{category.display_name}</h1>
				<p className="mt-1 text-sm text-neutral-500">
					<code className="rounded bg-neutral-100 px-1 font-mono">{category.slug}</code>
				</p>
			</header>

			{items.length === 0 ? (
				<p className="text-sm italic text-neutral-500">No pages in this category yet.</p>
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
