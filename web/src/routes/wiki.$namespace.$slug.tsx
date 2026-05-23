//! Namespace-aware page view (`/wiki/$namespace/$slug`) — added in #28.
//!
//! Mirrors `wiki.$slug.tsx` but reads the namespace from the URL so a wiki
//! with multiple namespaces (`Main`, `Help`, `User`, …) can address every
//! page unambiguously. The legacy `/wiki/$slug` route stays alive and
//! resolves against the implicit `Main` namespace for backwards compat.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { useMemo, useState } from "react";
import toast from "react-hot-toast";
import {
	addToWatchlist,
	ApiError,
	type AuthMePayload,
	canEditAtProtectionLevel,
	fetchAuthMe,
	fetchPage,
	listWatchlist,
	type PageView,
	type ProtectionLevel,
	parsePermissions,
	protectPage,
	removeFromWatchlist,
	type WatchlistResponse,
} from "../lib/api";
import { renderMarkdown } from "../lib/markdown";

export const Route = createFileRoute("/wiki/$namespace/$slug")({
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

interface ProtectionLabel {
	label: string;
	hint: string;
}

function protectionLabel(level: ProtectionLevel): ProtectionLabel {
	switch (level) {
		case "none":
			return { label: "Open", hint: "Anyone with edit access can change this page." };
		case "semi_protected":
			return { label: "Semi-protected", hint: "Semi-protected — log in to edit." };
		case "protected":
			return { label: "Protected", hint: "Protected — only editors can change this page." };
		case "fully_protected":
			return {
				label: "Fully protected",
				hint: "Fully protected — only administrators can change this page.",
			};
	}
}

function PageViewComponent() {
	const { namespace, slug } = Route.useParams();
	const queryClient = useQueryClient();

	const query = useQuery<PageView, ApiError>({
		queryKey: ["page", namespace, slug],
		queryFn: () => fetchPage(slug, namespace),
		retry: (failureCount, error) => {
			if (error instanceof ApiError && error.status === 404) {
				return false;
			}
			return failureCount < 1;
		},
	});

	const me = useQuery<AuthMePayload | null, ApiError>({
		queryKey: ["auth", "me"],
		queryFn: fetchAuthMe,
		retry: false,
		staleTime: 60_000,
	});

	const renderedHtml = useMemo(() => {
		if (!query.data) {
			return "";
		}
		return renderMarkdown(query.data.content);
	}, [query.data]);

	const [showProtectionModal, setShowProtectionModal] = useState(false);

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
			return <PageNotFound namespace={namespace} slug={slug} />;
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
	const protection = protectionLabel(page.protection_level);
	const authenticated = me.data !== null && me.data !== undefined;
	const permissions = parsePermissions(me.data?.permissions);
	const canEdit = canEditAtProtectionLevel(page.protection_level, authenticated, permissions);
	const canManageProtection = permissions.has("PROTECT");

	let editDisabledReason: string | null = null;
	if (!canEdit) {
		if (page.protection_level === "semi_protected" && !authenticated) {
			editDisabledReason = "Semi-protected — log in to edit.";
		} else if (page.protection_level === "protected") {
			editDisabledReason = "Protected — you need the editor role to edit this page.";
		} else if (page.protection_level === "fully_protected") {
			editDisabledReason = "Fully protected — only administrators can edit this page.";
		}
	}

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
					<div className="mt-8 flex flex-wrap gap-3 border-t border-neutral-200 pt-4">
						{canEdit ? (
							<Link
								to="/wiki/$namespace/$slug/edit"
								params={{ namespace, slug: page.slug }}
								className="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-neutral-800"
							>
								Edit
							</Link>
						) : (
							<button
								type="button"
								disabled
								title={editDisabledReason ?? undefined}
								aria-label={editDisabledReason ?? "Editing is disabled"}
								className="cursor-not-allowed rounded-md bg-neutral-200 px-3 py-1.5 text-sm font-medium text-neutral-500"
							>
								Edit
							</button>
						)}
						{authenticated && <WatchToggle pageId={page.id} />}
						<Link
							to="/wiki"
							className="rounded-md border border-neutral-300 bg-white px-3 py-1.5 text-sm font-medium text-neutral-800 hover:bg-neutral-100"
						>
							All pages
						</Link>
					</div>
					{editDisabledReason !== null && (
						<p className="mt-2 text-xs text-neutral-500">{editDisabledReason}</p>
					)}
				</article>

				<aside className="flex flex-col gap-4 rounded-md border border-neutral-200 bg-white p-4 text-sm">
					<div>
						<h2 className="text-xs font-medium uppercase tracking-wide text-neutral-500">
							Protection
						</h2>
						<p className="mt-1 flex items-center gap-1.5 text-neutral-800">
							<LockIcon level={page.protection_level} />
							<span>{protection.label}</span>
						</p>
						{page.protection_level !== "none" && (
							<p className="mt-1 text-xs text-neutral-500">{protection.hint}</p>
						)}
					</div>
					<div>
						<h2 className="text-xs font-medium uppercase tracking-wide text-neutral-500">
							Last edited
						</h2>
						<p className="mt-1 text-neutral-800">{formatTimestamp(page.updated_at)}</p>
					</div>
					<div>
						<Link
							to="/wiki/$namespace/$slug/history"
							params={{ namespace, slug: page.slug }}
							className="text-xs font-medium text-neutral-700 underline hover:text-neutral-900"
						>
							View history →
						</Link>
					</div>
					{canManageProtection && (
						<div className="border-t border-neutral-200 pt-3">
							<button
								type="button"
								onClick={() => setShowProtectionModal(true)}
								className="rounded-md border border-neutral-300 bg-white px-2.5 py-1 text-xs font-medium text-neutral-700 hover:bg-neutral-50"
							>
								Manage protection
							</button>
						</div>
					)}
				</aside>
			</div>

			{showProtectionModal && (
				<ProtectionModal
					page={page}
					namespace={namespace}
					onClose={() => setShowProtectionModal(false)}
					onSaved={(updated) => {
						queryClient.setQueryData<PageView>(["page", namespace, slug], updated);
						setShowProtectionModal(false);
						toast.success("Protection updated");
					}}
				/>
			)}
		</main>
	);
}

function LockIcon({ level }: { level: ProtectionLevel }) {
	if (level === "none") {
		return <span aria-hidden className="inline-block w-3" />;
	}
	const fill =
		level === "fully_protected"
			? "text-red-600"
			: level === "protected"
				? "text-amber-600"
				: "text-neutral-500";
	return (
		<svg
			aria-hidden
			viewBox="0 0 16 16"
			className={`inline-block h-3.5 w-3.5 ${fill}`}
			fill="currentColor"
		>
			<title>{`${level} lock icon`}</title>
			<path d="M8 1a3 3 0 0 0-3 3v3H4a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1V8a1 1 0 0 0-1-1h-1V4a3 3 0 0 0-3-3zm-2 6V4a2 2 0 1 1 4 0v3H6z" />
		</svg>
	);
}

const PROTECTION_LEVELS: ProtectionLevel[] = [
	"none",
	"semi_protected",
	"protected",
	"fully_protected",
];

interface ProtectionModalProps {
	page: PageView;
	namespace: string;
	onClose: () => void;
	onSaved: (updated: PageView) => void;
}

function ProtectionModal({ page, namespace, onClose, onSaved }: ProtectionModalProps) {
	const [selected, setSelected] = useState<ProtectionLevel>(page.protection_level);
	const mutation = useMutation<PageView, ApiError, ProtectionLevel>({
		mutationFn: (level) => protectPage(page.slug, { protection_level: level }, namespace),
		onSuccess: (data) => onSaved(data),
		onError: (error) => {
			toast.error(`Couldn’t update protection: ${error.message}`);
		},
	});

	return (
		<div
			className="fixed inset-0 z-10 flex items-center justify-center bg-black/40 px-4"
			role="dialog"
			aria-modal="true"
			aria-labelledby="protection-modal-title"
		>
			<div className="w-full max-w-md rounded-md border border-neutral-200 bg-white p-5 shadow-lg">
				<h2 id="protection-modal-title" className="text-lg font-semibold tracking-tight">
					Manage protection
				</h2>
				<p className="mt-1 text-sm text-neutral-600">
					Set the minimum role required to edit{" "}
					<code className="rounded bg-neutral-100 px-1 font-mono text-xs">
						{page.namespace_slug}/{page.slug}
					</code>
					.
				</p>
				<fieldset className="mt-4 flex flex-col gap-2">
					{PROTECTION_LEVELS.map((level) => {
						const meta = protectionLabel(level);
						return (
							<label
								key={level}
								className={`flex cursor-pointer items-start gap-3 rounded-md border px-3 py-2 ${
									selected === level
										? "border-neutral-900 bg-neutral-50"
										: "border-neutral-200 bg-white hover:border-neutral-400"
								}`}
							>
								<input
									type="radio"
									name="protection_level"
									value={level}
									checked={selected === level}
									onChange={() => setSelected(level)}
									className="mt-1"
								/>
								<span className="flex flex-col gap-0.5">
									<span className="text-sm font-medium text-neutral-900">{meta.label}</span>
									<span className="text-xs text-neutral-600">{meta.hint}</span>
								</span>
							</label>
						);
					})}
				</fieldset>
				<div className="mt-5 flex justify-end gap-2">
					<button
						type="button"
						onClick={onClose}
						className="rounded-md border border-neutral-300 bg-white px-3 py-1.5 text-sm font-medium text-neutral-800 hover:bg-neutral-100"
					>
						Cancel
					</button>
					<button
						type="button"
						onClick={() => mutation.mutate(selected)}
						disabled={mutation.isPending || selected === page.protection_level}
						className="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-neutral-800 disabled:cursor-not-allowed disabled:opacity-60"
					>
						{mutation.isPending ? "Saving…" : "Save"}
					</button>
				</div>
			</div>
		</div>
	);
}

/**
 * Star/unstar button that toggles a page on the current user's watchlist.
 *
 * Reads the list once to decide whether the page is already watched, then
 * issues `POST`/`DELETE` on click and patches the cached list so the toggle
 * flips instantly. Mutations bubble errors through `react-hot-toast` so the
 * user sees a clear failure surface without an alert dialog.
 */
function WatchToggle({ pageId }: { pageId: string }) {
	const queryClient = useQueryClient();
	const query = useQuery<WatchlistResponse, ApiError>({
		queryKey: ["watchlist"],
		queryFn: listWatchlist,
		retry: (failureCount, error) => {
			if (error instanceof ApiError && error.status === 401) {
				return false;
			}
			return failureCount < 1;
		},
		staleTime: 30_000,
	});

	const watching = query.data?.items.some((row) => row.page_id === pageId) ?? false;

	const mutation = useMutation<void, ApiError, boolean>({
		mutationFn: async (nextWatching) => {
			if (nextWatching) {
				await addToWatchlist(pageId);
			} else {
				await removeFromWatchlist(pageId);
			}
		},
		onSuccess: (_void, nextWatching) => {
			queryClient.setQueryData<WatchlistResponse>(["watchlist"], (prev) => {
				if (!prev) {
					return prev;
				}
				if (nextWatching) {
					return prev; // refetch picks the new row up; nothing to merge inline.
				}
				return { items: prev.items.filter((row) => row.page_id !== pageId) };
			});
			void queryClient.invalidateQueries({ queryKey: ["watchlist"] });
			toast.success(nextWatching ? "Watching" : "Removed from watchlist");
		},
		onError: (error) => {
			toast.error(`Couldn't update watchlist: ${error.message}`);
		},
	});

	const disabled =
		query.isPending ||
		mutation.isPending ||
		(query.isError &&
			!(query.error instanceof ApiError && query.error.status === 401));

	return (
		<button
			type="button"
			onClick={() => mutation.mutate(!watching)}
			disabled={disabled}
			className={`inline-flex items-center gap-1.5 rounded-md border px-3 py-1.5 text-sm font-medium transition-colors ${
				watching
					? "border-amber-400 bg-amber-50 text-amber-800 hover:bg-amber-100"
					: "border-neutral-300 bg-white text-neutral-700 hover:bg-neutral-50"
			} disabled:cursor-not-allowed disabled:opacity-60`}
			aria-pressed={watching}
			title={watching ? "Stop watching this page" : "Add this page to your watchlist"}
		>
			<StarIcon filled={watching} />
			{watching ? "Watching" : "Watch"}
		</button>
	);
}

function StarIcon({ filled }: { filled: boolean }) {
	return (
		<svg
			aria-hidden
			viewBox="0 0 16 16"
			className={`h-3.5 w-3.5 ${filled ? "text-amber-500" : "text-neutral-400"}`}
			fill={filled ? "currentColor" : "none"}
			stroke="currentColor"
			strokeWidth="1.5"
			strokeLinejoin="round"
		>
			<title>{filled ? "Watching" : "Not watching"}</title>
			<path d="M8 1.5l1.95 4.32 4.55.59-3.4 3.18.92 4.91L8 12.06l-4.02 2.44.92-4.91L1.5 6.4l4.55-.59L8 1.5z" />
		</svg>
	);
}

function PageNotFound({ namespace, slug }: { namespace: string; slug: string }) {
	return (
		<main className="mx-auto max-w-2xl px-6 py-16 text-center">
			<p className="font-mono text-xs uppercase tracking-wide text-neutral-500">404</p>
			<h1 className="mt-2 text-3xl font-semibold tracking-tight">Page not found</h1>
			<p className="mt-2 text-neutral-600">
				No page{" "}
				<code className="rounded bg-neutral-200 px-1 font-mono">
					{namespace}/{slug}
				</code>{" "}
				exists yet.
			</p>
			<div className="mt-6 flex justify-center gap-3">
				<Link
					to="/wiki/$namespace/$slug/edit"
					params={{ namespace, slug }}
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
