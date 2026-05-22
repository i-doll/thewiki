import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { useMemo, useState } from "react";
import toast from "react-hot-toast";
import {
	ApiError,
	type AuthMePayload,
	canEditAtProtectionLevel,
	fetchAuthMe,
	fetchPage,
	type PageView,
	type ProtectionLevel,
	parsePermissions,
	protectPage,
} from "../lib/api";
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

interface ProtectionLabel {
	label: string;
	hint: string;
}

function protectionLabel(level: ProtectionLevel): ProtectionLabel {
	switch (level) {
		case "none":
			return { label: "Open", hint: "Anyone with edit access can change this page." };
		case "semi_protected":
			return {
				label: "Semi-protected",
				hint: "Semi-protected — log in to edit.",
			};
		case "protected":
			return {
				label: "Protected",
				hint: "Protected — only editors can change this page.",
			};
		case "fully_protected":
			return {
				label: "Fully protected",
				hint: "Fully protected — only administrators can change this page.",
			};
	}
}

function PageViewComponent() {
	const { slug } = Route.useParams();
	const queryClient = useQueryClient();

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

	// Fetch the calling user separately so the page render isn't blocked on
	// the auth lookup. A logged-out caller resolves to `null` (not an
	// error) so we can branch without `try`/`catch`.
	const me = useQuery<AuthMePayload | null, ApiError>({
		queryKey: ["auth", "me"],
		queryFn: fetchAuthMe,
		retry: false,
		// Cache for the session — /me is cheap but unnecessary to refetch
		// on every render.
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
	const protection = protectionLabel(page.protection_level);
	const authenticated = me.data !== null && me.data !== undefined;
	const permissions = parsePermissions(me.data?.permissions);
	const canEdit = canEditAtProtectionLevel(page.protection_level, authenticated, permissions);
	const canManageProtection = permissions.has("PROTECT");

	// Build a tooltip explaining why the edit button is disabled. Shown via
	// the native `title` attribute — accessible by default and consistent
	// with the rest of the SPA which doesn't ship a tooltip primitive.
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
					<div className="mt-8 flex gap-3 border-t border-neutral-200 pt-4">
						{canEdit ? (
							<Link
								to="/wiki/$slug/edit"
								params={{ slug: page.slug }}
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
					{page.categories.length > 0 && (
						<div>
							<h2 className="text-xs font-medium uppercase tracking-wide text-neutral-500">
								Categories
							</h2>
							<ul className="mt-1 flex flex-wrap gap-1.5">
								{page.categories.map((cat) => (
									<li key={cat.id}>
										<Link
											to="/category/$slug"
											params={{ slug: cat.slug }}
											className="inline-flex items-center rounded-full border border-neutral-300 bg-neutral-100 px-2 py-0.5 text-xs font-medium text-neutral-700 hover:bg-neutral-200"
										>
											{cat.display_name}
										</Link>
									</li>
								))}
							</ul>
						</div>
					)}
					{page.tags.length > 0 && (
						<div>
							<h2 className="text-xs font-medium uppercase tracking-wide text-neutral-500">Tags</h2>
							<ul className="mt-1 flex flex-wrap gap-1.5">
								{page.tags.map((tag) => (
									<li key={tag}>
										<Link
											to="/tag/$tag"
											params={{ tag }}
											className="inline-flex items-center rounded-full border border-neutral-200 bg-white px-2 py-0.5 text-xs font-mono text-neutral-700 hover:bg-neutral-100"
										>
											#{tag}
										</Link>
									</li>
								))}
							</ul>
						</div>
					)}
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
					onClose={() => setShowProtectionModal(false)}
					onSaved={(updated) => {
						queryClient.setQueryData<PageView>(["page", slug], updated);
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
		// Empty placeholder so the badge keeps a consistent width across rows.
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
	onClose: () => void;
	onSaved: (updated: PageView) => void;
}

function ProtectionModal({ page, onClose, onSaved }: ProtectionModalProps) {
	const [selected, setSelected] = useState<ProtectionLevel>(page.protection_level);
	const mutation = useMutation<PageView, ApiError, ProtectionLevel>({
		mutationFn: (level) => protectPage(page.slug, { protection_level: level }),
		onSuccess: (data) => {
			onSaved(data);
		},
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
					<code className="rounded bg-neutral-100 px-1 font-mono text-xs">{page.slug}</code>.
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
