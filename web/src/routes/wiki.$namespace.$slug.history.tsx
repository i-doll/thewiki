//! Namespace-aware history view (`/wiki/$namespace/$slug/history`) — added in #28.

import { useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { useMemo, useState } from "react";

export const Route = createFileRoute("/wiki/$namespace/$slug/history")({
	component: HistoryComponent,
});

const REVERT_ENABLED = false;
const DEFAULT_PAGE_SIZE = 20;
const USER_ID_STORAGE_KEY = "thewiki:user-id";

interface RevisionView {
	id: string;
	page_id: string;
	parent_id: string | null;
	author_id: string;
	edit_summary: string | null;
	body_excerpt: string;
	created_at: string;
}

interface RevisionListResponse {
	items: RevisionView[];
	next_cursor: string | null;
}

interface RevertResponse {
	id: string;
}

interface ApiErrorBody {
	message?: string;
}

class ApiError extends Error {
	override readonly name = "ApiError";
	readonly status: number;
	constructor(status: number, message: string) {
		super(message);
		this.status = status;
	}
}

async function fetchRevisions(
	namespace: string,
	slug: string,
	cursor: string | undefined,
): Promise<RevisionListResponse> {
	const params = new URLSearchParams();
	if (cursor) {
		params.set("cursor", cursor);
	}
	params.set("limit", String(DEFAULT_PAGE_SIZE));
	const url = `/api/v1/wiki/${encodeURIComponent(namespace)}/${encodeURIComponent(slug)}/revisions?${params.toString()}`;
	const res = await fetch(url);
	if (!res.ok) {
		let detail = res.statusText;
		try {
			const body = (await res.json()) as ApiErrorBody;
			if (body.message) {
				detail = body.message;
			}
		} catch {
			// non-JSON body
		}
		throw new ApiError(res.status, `Failed to load revisions: ${detail}`);
	}
	return (await res.json()) as RevisionListResponse;
}

async function postRevert(
	namespace: string,
	slug: string,
	fromRevision: string,
	message: string | null,
): Promise<RevertResponse> {
	const body: Record<string, unknown> = { from_revision: fromRevision };
	if (message !== null && message.length > 0) {
		body.message = message;
	}
	const res = await fetch(
		`/api/v1/wiki/${encodeURIComponent(namespace)}/${encodeURIComponent(slug)}/revert`,
		{
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(body),
		},
	);
	if (!res.ok) {
		let detail = res.statusText;
		try {
			const errBody = (await res.json()) as ApiErrorBody;
			if (errBody.message) {
				detail = errBody.message;
			}
		} catch {
			// non-JSON body
		}
		throw new ApiError(res.status, `Revert failed: ${detail}`);
	}
	return (await res.json()) as RevertResponse;
}

function isLoggedIn(): boolean {
	if (typeof window === "undefined") {
		return false;
	}
	try {
		const raw = window.localStorage.getItem(USER_ID_STORAGE_KEY);
		return typeof raw === "string" && raw.length > 0;
	} catch {
		return false;
	}
}

function formatRelative(iso: string): string {
	const then = new Date(iso).getTime();
	if (Number.isNaN(then)) {
		return iso;
	}
	const diffSeconds = Math.round((then - Date.now()) / 1000);
	const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
	const abs = Math.abs(diffSeconds);
	if (abs < 60) {
		return formatter.format(diffSeconds, "second");
	}
	if (abs < 3600) {
		return formatter.format(Math.round(diffSeconds / 60), "minute");
	}
	if (abs < 86_400) {
		return formatter.format(Math.round(diffSeconds / 3600), "hour");
	}
	if (abs < 2_592_000) {
		return formatter.format(Math.round(diffSeconds / 86_400), "day");
	}
	if (abs < 31_536_000) {
		return formatter.format(Math.round(diffSeconds / 2_592_000), "month");
	}
	return formatter.format(Math.round(diffSeconds / 31_536_000), "year");
}

function formatAbsolute(iso: string): string {
	const date = new Date(iso);
	if (Number.isNaN(date.getTime())) {
		return iso;
	}
	return date.toLocaleString();
}

function shortenId(id: string): string {
	return id.length <= 8 ? id : id.slice(0, 8);
}

function HistoryComponent() {
	const { namespace, slug } = Route.useParams();
	const navigate = useNavigate();
	const queryClient = useQueryClient();
	const [revertError, setRevertError] = useState<string | null>(null);

	const query = useInfiniteQuery({
		queryKey: ["revisions", namespace, slug],
		queryFn: ({ pageParam }) => fetchRevisions(namespace, slug, pageParam),
		initialPageParam: undefined as string | undefined,
		getNextPageParam: (last) => last.next_cursor ?? undefined,
	});

	const revisions = useMemo(() => query.data?.pages.flatMap((p) => p.items) ?? [], [query.data]);
	const loggedIn = isLoggedIn();

	const revertMutation = useMutation({
		mutationFn: (args: { revisionId: string; message: string | null }) =>
			postRevert(namespace, slug, args.revisionId, args.message),
		onSuccess: async () => {
			setRevertError(null);
			await queryClient.invalidateQueries({ queryKey: ["page", namespace, slug] });
			await queryClient.invalidateQueries({ queryKey: ["revisions", namespace, slug] });
			navigate({
				to: "/wiki/$namespace/$slug",
				params: { namespace, slug },
			}).catch(() => {
				// fall back silently
			});
		},
		onError: (err: unknown) => {
			const message = err instanceof Error ? err.message : "Revert failed";
			setRevertError(message);
		},
	});

	const onRevert = (revision: RevisionView) => {
		if (!REVERT_ENABLED || !loggedIn) {
			return;
		}
		const promptFn = typeof window !== "undefined" ? window.prompt : null;
		const message = promptFn
			? promptFn("Optional message describing this revert:", `Revert to ${shortenId(revision.id)}`)
			: null;
		if (message === null) {
			return;
		}
		revertMutation.mutate({ revisionId: revision.id, message });
	};

	return (
		<main className="mx-auto flex max-w-4xl flex-col gap-6 px-6 py-10">
			<header className="flex flex-col gap-1">
				<nav className="text-xs text-neutral-500">
					<Link to="/" className="hover:text-neutral-700">
						home
					</Link>
					{" / "}
					<span>wiki</span>
					{" / "}
					<span className="font-mono">{namespace}</span>
					{" / "}
					<span className="font-mono">{slug}</span>
					{" / "}
					<span>history</span>
				</nav>
				<h1 className="text-2xl font-semibold tracking-tight">
					History —{" "}
					<span className="font-mono">
						{namespace}/{slug}
					</span>
				</h1>
				<p className="text-sm text-neutral-600">
					Revisions newest first. Click any row's diff link to compare it against another revision.
				</p>
			</header>

			{query.isPending && (
				<div className="rounded-md border border-neutral-200 bg-white p-4 text-sm text-neutral-500">
					Loading revisions…
				</div>
			)}

			{query.isError && (
				<div className="rounded-md border border-red-200 bg-red-50 p-4 text-sm text-red-700">
					{query.error instanceof Error ? query.error.message : "Failed to load revisions"}
				</div>
			)}

			{revertError && (
				<div className="rounded-md border border-red-200 bg-red-50 p-4 text-sm text-red-700">
					{revertError}
				</div>
			)}

			{query.isSuccess && revisions.length === 0 && (
				<div className="rounded-md border border-neutral-200 bg-white p-4 text-sm text-neutral-500">
					No revisions yet.
				</div>
			)}

			{revisions.length > 0 && (
				<ol className="flex flex-col gap-3">
					{revisions.map((rev, index) => {
						const previousId = rev.parent_id;
						const headId = revisions[0]?.id ?? rev.id;
						const isHead = index === 0;
						return (
							<li
								key={rev.id}
								className="flex flex-col gap-2 rounded-md border border-neutral-200 bg-white p-4"
							>
								<div className="flex flex-wrap items-center justify-between gap-2">
									<div className="flex flex-col gap-0.5">
										<span
											className="text-sm font-medium text-neutral-800"
											title={formatAbsolute(rev.created_at)}
										>
											{formatRelative(rev.created_at)}
										</span>
										<span className="font-mono text-xs text-neutral-500">
											{shortenId(rev.id)} · author {shortenId(rev.author_id)}
											{isHead && " · head"}
										</span>
									</div>
									<div className="flex flex-wrap gap-2 text-xs">
										{previousId ? (
											<Link
												to="/wiki/$namespace/$slug/diff"
												params={{ namespace, slug }}
												search={{ from: previousId, to: rev.id }}
												className="rounded border border-neutral-300 px-2 py-1 text-neutral-700 hover:bg-neutral-100"
											>
												diff with previous
											</Link>
										) : (
											<span
												className="rounded border border-neutral-200 px-2 py-1 text-neutral-400"
												title="No earlier revision"
											>
												diff with previous
											</span>
										)}
										{!isHead && (
											<Link
												to="/wiki/$namespace/$slug/diff"
												params={{ namespace, slug }}
												search={{ from: rev.id, to: headId }}
												className="rounded border border-neutral-300 px-2 py-1 text-neutral-700 hover:bg-neutral-100"
											>
												diff with current
											</Link>
										)}
										<button
											type="button"
											disabled={!REVERT_ENABLED || !loggedIn || revertMutation.isPending}
											onClick={() => onRevert(rev)}
											title={
												!REVERT_ENABLED
													? "revert coming soon"
													: !loggedIn
														? "sign in to revert"
														: "revert page to this revision"
											}
											className="rounded border border-neutral-300 px-2 py-1 text-neutral-700 hover:bg-neutral-100 disabled:cursor-not-allowed disabled:bg-neutral-50 disabled:text-neutral-400"
										>
											revert
										</button>
									</div>
								</div>
								{rev.edit_summary && <p className="text-sm text-neutral-700">{rev.edit_summary}</p>}
								{rev.body_excerpt && (
									<p className="line-clamp-3 whitespace-pre-wrap font-mono text-xs text-neutral-500">
										{rev.body_excerpt}
									</p>
								)}
							</li>
						);
					})}
				</ol>
			)}

			{query.isSuccess && (
				<div className="flex justify-center">
					{query.hasNextPage ? (
						<button
							type="button"
							onClick={() => query.fetchNextPage()}
							disabled={query.isFetchingNextPage}
							className="rounded-md border border-neutral-300 bg-white px-4 py-2 text-sm text-neutral-700 hover:bg-neutral-100 disabled:cursor-not-allowed disabled:text-neutral-400"
						>
							{query.isFetchingNextPage ? "Loading…" : "Load older revisions"}
						</button>
					) : (
						revisions.length > 0 && (
							<span className="text-xs text-neutral-400">No more revisions.</span>
						)
					)}
				</div>
			)}
		</main>
	);
}
