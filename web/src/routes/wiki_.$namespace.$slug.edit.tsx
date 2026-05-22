//! Namespace-aware editor (`/wiki/$namespace/$slug/edit`) — added in #28.
//!
//! Mirrors `wiki_.$slug.edit.tsx`. Reads the namespace from the path so
//! create / update mutations target the right namespace.

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

export const Route = createFileRoute("/wiki_/$namespace/$slug/edit")({
	component: PageEditComponent,
	validateSearch: (search: Record<string, unknown>): EditSearch => {
		return search.new === 1 || search.new === "1" ? { new: 1 } : {};
	},
});

function PageEditComponent() {
	const { namespace, slug } = Route.useParams();
	const { new: isNewFlag } = Route.useSearch();
	const isNew = isNewFlag === 1;
	const queryClient = useQueryClient();
	const navigate = useNavigate();

	const existing = useQuery<PageView, ApiError>({
		queryKey: ["page", namespace, slug],
		queryFn: () => fetchPage(slug, namespace),
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
					namespace_slug: namespace,
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
			return updatePage(slug, body, namespace);
		},
		onMutate: async ({ title: nextTitle, content: nextContent }) => {
			await queryClient.cancelQueries({ queryKey: ["page", namespace, slug] });
			const previous = queryClient.getQueryData<PageView>(["page", namespace, slug]);
			if (previous) {
				queryClient.setQueryData<PageView>(["page", namespace, slug], {
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
				queryClient.setQueryData(["page", namespace, slug], context.previous);
			}
			toast.error(`Save failed: ${error.message}`);
		},
		onSuccess: (data) => {
			queryClient.setQueryData(["page", namespace, slug], data);
			toast.success(isCreating ? "Page created" : "Page saved");
		},
		onSettled: () => {
			queryClient.invalidateQueries({ queryKey: ["page", namespace, slug] });
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
					if (isCreating) {
						navigate({
							to: "/wiki/$namespace/$slug",
							params: { namespace, slug: data.slug },
						});
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
						{namespace} / {slug}
					</p>
					<h1 className="text-2xl font-semibold tracking-tight">
						{isCreating ? "Create page" : "Edit page"}
					</h1>
				</div>
				<Link
					to="/wiki/$namespace/$slug"
					params={{ namespace, slug }}
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
						to="/wiki/$namespace/$slug"
						params={{ namespace, slug }}
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
