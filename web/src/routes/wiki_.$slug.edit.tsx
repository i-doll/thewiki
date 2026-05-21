import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import toast from "react-hot-toast";
import { Editor } from "../components/Editor";
import {
	ApiError,
	type CreatePageRequest,
	createPage,
	fetchPage,
	type PageView,
	type UpdatePageRequest,
	updatePage,
} from "../lib/api";

interface EditSearch {
	new?: 1;
}

/** Namespace assumed for all writes until #28 lands prefix routing. */
const DEFAULT_NAMESPACE = "Main";

export const Route = createFileRoute("/wiki_/$slug/edit")({
	component: PageEditComponent,
	validateSearch: (search: Record<string, unknown>): EditSearch => {
		// Only `?new=1` is meaningful; everything else is dropped so links stay
		// canonical.
		return search.new === 1 || search.new === "1" ? { new: 1 } : {};
	},
});

function PageEditComponent() {
	const { slug } = Route.useParams();
	const { new: isNewFlag } = Route.useSearch();
	const isNew = isNewFlag === 1;
	const queryClient = useQueryClient();
	const navigate = useNavigate();

	// We treat the existence query as authoritative: if `?new=1` is set we
	// skip the GET entirely; otherwise we fetch and let a 404 fall through
	// to "create" mode (the user might have navigated to /edit directly).
	const existing = useQuery<PageView, ApiError>({
		queryKey: ["page", slug],
		queryFn: () => fetchPage(slug),
		enabled: !isNew,
		retry: (failureCount, error) => {
			if (error instanceof ApiError && error.status === 404) {
				return false;
			}
			return failureCount < 1;
		},
	});

	const [title, setTitle] = useState<string>("");
	const [content, setContent] = useState<string>("");
	const [hydrated, setHydrated] = useState<boolean>(false);

	// Sync local form state from the fetched page exactly once — re-running
	// after every keystroke would clobber the user's edits.
	useEffect(() => {
		if (hydrated) {
			return;
		}
		if (isNew) {
			setTitle((current) => (current === "" ? slug : current));
			setHydrated(true);
			return;
		}
		if (existing.isSuccess) {
			setTitle(existing.data.title);
			setContent(existing.data.content);
			setHydrated(true);
		} else if (existing.isError && existing.error.status === 404) {
			// Page doesn't exist — fall back to create mode without losing the
			// user's typed slug.
			setTitle(slug);
			setHydrated(true);
		}
	}, [hydrated, isNew, slug, existing.isSuccess, existing.isError, existing.data, existing.error]);

	const isCreating = isNew || (existing.isError && existing.error?.status === 404);

	const mutation = useMutation<
		PageView,
		ApiError,
		{ title: string; content: string },
		{ previous: PageView | undefined }
	>({
		mutationFn: async ({ title: nextTitle, content: nextContent }) => {
			if (isCreating) {
				const body: CreatePageRequest = {
					namespace_slug: DEFAULT_NAMESPACE,
					slug,
					title: nextTitle,
					content: nextContent,
				};
				return createPage(body);
			}
			const body: UpdatePageRequest = {
				title: nextTitle,
				content: nextContent,
			};
			return updatePage(slug, body);
		},
		onMutate: async ({ title: nextTitle, content: nextContent }) => {
			await queryClient.cancelQueries({ queryKey: ["page", slug] });
			const previous = queryClient.getQueryData<PageView>(["page", slug]);
			if (previous) {
				// Optimistic update: stamp `updated_at` so the view route shows
				// the new write straight away even if the server is slow.
				queryClient.setQueryData<PageView>(["page", slug], {
					...previous,
					title: nextTitle,
					content: nextContent,
					updated_at: new Date().toISOString(),
				});
			}
			return { previous };
		},
		onError: (error, _vars, context) => {
			if (context?.previous) {
				queryClient.setQueryData(["page", slug], context.previous);
			}
			toast.error(`Save failed: ${error.message}`);
		},
		onSuccess: (data) => {
			queryClient.setQueryData(["page", slug], data);
			toast.success(isCreating ? "Page created" : "Page saved");
		},
		onSettled: () => {
			queryClient.invalidateQueries({ queryKey: ["page", slug] });
			if (isCreating) {
				queryClient.invalidateQueries({ queryKey: ["pages", "list"] });
			}
		},
	});

	const onSave = () => {
		if (title.trim().length === 0) {
			toast.error("Title must not be empty");
			return;
		}
		mutation.mutate(
			{ title: title.trim(), content },
			{
				onSuccess: (data) => {
					// After a successful create, hop straight to the view route so
					// the user lands on rendered content rather than the empty editor.
					if (isCreating) {
						navigate({ to: "/wiki/$slug", params: { slug: data.slug } });
					}
				},
			},
		);
	};

	if (!hydrated && !isNew && existing.isPending) {
		return (
			<main className="mx-auto max-w-3xl px-6 py-10">
				<div className="flex flex-col gap-3">
					<div className="h-8 w-1/2 animate-pulse rounded bg-neutral-200" />
					<div className="h-48 w-full animate-pulse rounded bg-neutral-200" />
				</div>
			</main>
		);
	}

	if (!isNew && existing.isError && existing.error.status !== 404) {
		return (
			<main className="mx-auto max-w-3xl px-6 py-10">
				<div className="rounded-md border border-red-300 bg-red-50 p-4 text-sm text-red-700">
					Failed to load page for editing: {existing.error.message}
				</div>
			</main>
		);
	}

	return (
		<main className="mx-auto flex max-w-3xl flex-col gap-4 px-6 py-10">
			<header className="flex items-baseline justify-between">
				<div>
					<p className="font-mono text-xs text-neutral-500">
						{DEFAULT_NAMESPACE} / {slug}
					</p>
					<h1 className="text-2xl font-semibold tracking-tight">
						{isCreating ? "Create page" : "Edit page"}
					</h1>
				</div>
				<Link
					to="/wiki/$slug"
					params={{ slug }}
					className="text-sm text-neutral-600 hover:text-neutral-900"
				>
					Cancel
				</Link>
			</header>

			<label className="flex flex-col gap-1 text-sm">
				<span className="font-medium text-neutral-700">Title</span>
				<input
					type="text"
					value={title}
					onChange={(event) => setTitle(event.target.value)}
					className="rounded-md border border-neutral-300 bg-white px-3 py-2 text-sm focus:border-neutral-500 focus:outline-none"
					placeholder="Page title"
				/>
			</label>

			<div className="flex flex-col gap-1 text-sm">
				<span className="font-medium text-neutral-700">Content</span>
				<Editor value={content} onChange={setContent} />
			</div>

			<div className="flex items-center justify-between border-t border-neutral-200 pt-4">
				<p className="text-xs text-neutral-500">
					{mutation.isPending
						? "Saving…"
						: isCreating
							? "Will create a new page on save."
							: "Saves commit a new revision."}
				</p>
				<div className="flex gap-3">
					<Link
						to="/wiki/$slug"
						params={{ slug }}
						className="rounded-md border border-neutral-300 bg-white px-3 py-1.5 text-sm font-medium text-neutral-800 hover:bg-neutral-100"
					>
						Cancel
					</Link>
					<button
						type="button"
						onClick={onSave}
						disabled={mutation.isPending}
						className="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-neutral-800 disabled:cursor-not-allowed disabled:opacity-60"
					>
						{isCreating ? "Create page" : "Save changes"}
					</button>
				</div>
			</div>

			{mutation.isError && (
				<div className="rounded-md border border-red-300 bg-red-50 p-3 text-sm text-red-700">
					{mutation.error.message}
				</div>
			)}
		</main>
	);
}
