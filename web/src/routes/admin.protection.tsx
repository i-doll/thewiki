//! Page protection manager (`/admin/protection`) — #47.
//!
//! Search pages, pick a protection level, apply singly or in bulk. The
//! per-page protect endpoint is at
//! `/api/v1/wiki/{namespace}/{slug}/protect` — we route through that for
//! both the single and bulk cases.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { ConfirmDialog } from "../components/ConfirmDialog";
import {
	type AuthMePayload,
	fetchAuthMe,
	fetchPage,
	type PageView,
	type ProtectionLevel,
	parsePermissions,
	protectPage,
} from "../lib/api";
import { type SearchHit, type SearchResponse, searchPages } from "../lib/search";

export const Route = createFileRoute("/admin/protection")({
	component: ProtectionComponent,
});

const LEVELS: { value: ProtectionLevel; label: string }[] = [
	{ value: "none", label: "None" },
	{ value: "semi_protected", label: "Semi-protected" },
	{ value: "protected", label: "Protected" },
	{ value: "fully_protected", label: "Fully protected" },
];

function ProtectionComponent() {
	const meQuery = useQuery<AuthMePayload | null>({
		queryKey: ["auth-me"],
		queryFn: fetchAuthMe,
	});
	if (meQuery.isPending) return <Skeleton />;
	if (meQuery.isError) return <ErrorMain message={meQuery.error.message} />;
	const me = meQuery.data;
	if (!me) return <ErrorMain message="You must sign in to manage protection." />;
	const perms = parsePermissions(me.permissions);
	if (!perms.has("PROTECT")) return <ErrorMain message="You do not have the PROTECT permission." />;
	return <ProtectionPanel />;
}

interface PageRow {
	page_id: string;
	namespace_slug: string;
	slug: string;
	title: string;
	protection_level: ProtectionLevel | "unknown";
}

function ProtectionPanel() {
	const qc = useQueryClient();
	const [query, setQuery] = useState("");
	const [selected, setSelected] = useState<Set<string>>(new Set());
	const [bulkLevel, setBulkLevel] = useState<ProtectionLevel>("none");
	const [confirm, setConfirm] = useState<
		| { kind: "single"; row: PageRow; level: ProtectionLevel }
		| { kind: "bulk"; rows: PageRow[]; level: ProtectionLevel }
		| null
	>(null);

	const searchQuery = useQuery<SearchResponse, Error>({
		queryKey: ["admin-protection-search", query],
		queryFn: () => searchPages(query, { limit: 25 }),
		enabled: query.trim().length > 0,
	});

	// We need protection_level for each hit, which the search endpoint
	// doesn't return. Fetch the full PageView for each row lazily once a
	// search lands; results are cached per (namespace, slug).
	const hits = searchQuery.data?.items ?? [];

	const protectMutation = useMutation({
		mutationFn: async (args: { rows: PageRow[]; level: ProtectionLevel }) => {
			const out: PageView[] = [];
			for (const row of args.rows) {
				out.push(await protectPage(row.slug, { protection_level: args.level }, row.namespace_slug));
			}
			return out;
		},
		onSuccess: () => {
			setSelected(new Set());
			setConfirm(null);
			void qc.invalidateQueries({ queryKey: ["admin-protection-search"] });
			void qc.invalidateQueries({ queryKey: ["admin-protection-page"] });
		},
	});

	const toggleOne = (id: string) => {
		const next = new Set(selected);
		if (next.has(id)) next.delete(id);
		else next.add(id);
		setSelected(next);
	};
	const toggleAll = () => {
		if (selected.size === hits.length) {
			setSelected(new Set());
		} else {
			setSelected(new Set(hits.map((h) => h.page_id)));
		}
	};

	const selectedHits = hits.filter((h) => selected.has(h.page_id));

	return (
		<main className="mx-auto max-w-5xl px-6 py-10">
			<header className="mb-6 border-b border-neutral-200 pb-4">
				<p className="font-mono text-xs uppercase tracking-wide text-neutral-500">Admin</p>
				<h1 className="mt-1 text-3xl font-semibold tracking-tight">Page protection</h1>
				<p className="mt-2 text-sm text-neutral-600">
					Find pages and adjust their protection level. Bulk-select rows to apply the same level
					across them.
				</p>
			</header>

			<section className="mb-4 rounded-md border border-neutral-200 bg-neutral-50 p-4">
				<input
					type="search"
					value={query}
					onChange={(e) => setQuery(e.target.value)}
					placeholder="Search pages…"
					className="w-full rounded border border-neutral-300 px-3 py-2 text-sm"
				/>
			</section>

			{selected.size > 0 && (
				<section className="mb-4 flex items-center gap-3 rounded-md border border-neutral-200 bg-amber-50 px-4 py-3 text-sm">
					<span>
						<strong>{selected.size}</strong> page{selected.size === 1 ? "" : "s"} selected
					</span>
					<select
						value={bulkLevel}
						onChange={(e) => setBulkLevel(e.target.value as ProtectionLevel)}
						className="rounded border border-neutral-300 bg-white px-2 py-1 text-sm"
					>
						{LEVELS.map((l) => (
							<option key={l.value} value={l.value}>
								{l.label}
							</option>
						))}
					</select>
					<button
						type="button"
						onClick={() => {
							const rows: PageRow[] = selectedHits.map((h) => ({
								page_id: h.page_id,
								namespace_slug: h.namespace_slug,
								slug: h.slug,
								title: h.title,
								protection_level: "unknown",
							}));
							setConfirm({ kind: "bulk", rows, level: bulkLevel });
						}}
						className="rounded bg-neutral-900 px-3 py-1 text-xs font-medium text-white"
					>
						Apply
					</button>
				</section>
			)}

			{searchQuery.isError && <ErrorBox>{searchQuery.error.message}</ErrorBox>}
			{searchQuery.isPending && query.trim().length > 0 && <Skeleton />}
			{query.trim().length === 0 && (
				<p className="text-sm italic text-neutral-500">Start typing to search pages.</p>
			)}
			{searchQuery.data && hits.length === 0 && (
				<p className="text-sm italic text-neutral-500">No pages match.</p>
			)}
			{hits.length > 0 && (
				<div className="overflow-x-auto rounded-md border border-neutral-200 bg-white">
					<table className="min-w-full divide-y divide-neutral-200 text-left">
						<thead>
							<tr>
								<th className="w-10 px-3 py-2">
									<input
										type="checkbox"
										aria-label="Select all results"
										checked={hits.length > 0 && selected.size === hits.length}
										onChange={toggleAll}
									/>
								</th>
								<HeaderCell>Title</HeaderCell>
								<HeaderCell>Namespace / slug</HeaderCell>
								<HeaderCell>Current level</HeaderCell>
								<HeaderCell>Set to</HeaderCell>
							</tr>
						</thead>
						<tbody className="divide-y divide-neutral-100">
							{hits.map((hit) => (
								<PageRowComponent
									key={hit.page_id}
									hit={hit}
									checked={selected.has(hit.page_id)}
									onToggle={() => toggleOne(hit.page_id)}
									onProtect={(level) =>
										setConfirm({
											kind: "single",
											row: {
												page_id: hit.page_id,
												namespace_slug: hit.namespace_slug,
												slug: hit.slug,
												title: hit.title,
												protection_level: "unknown",
											},
											level,
										})
									}
								/>
							))}
						</tbody>
					</table>
				</div>
			)}

			<ConfirmDialog
				open={confirm !== null}
				title="Change protection level?"
				message={(() => {
					if (!confirm) return "";
					if (confirm.kind === "single") {
						return `Set ${confirm.row.namespace_slug}/${confirm.row.slug} to "${labelFor(
							confirm.level,
						)}"?`;
					}
					return `Set ${confirm.rows.length} page(s) to "${labelFor(confirm.level)}"? This change is logged in the audit trail.`;
				})()}
				confirmLabel="Apply"
				busy={protectMutation.isPending}
				onConfirm={() => {
					if (!confirm) return;
					const rows = confirm.kind === "single" ? [confirm.row] : confirm.rows;
					protectMutation.mutate({ rows, level: confirm.level });
				}}
				onCancel={() => setConfirm(null)}
			/>
			{protectMutation.isError && (
				<p className="mt-3 text-sm text-red-700">{(protectMutation.error as Error).message}</p>
			)}
		</main>
	);
}

function PageRowComponent({
	hit,
	checked,
	onToggle,
	onProtect,
}: {
	hit: SearchHit;
	checked: boolean;
	onToggle: () => void;
	onProtect: (level: ProtectionLevel) => void;
}) {
	// Pull the protection level lazily — the search endpoint doesn't
	// include it. One PageView fetch per visible row is fine for the
	// admin volume the spec calls out.
	const pageQuery = useQuery<PageView, Error>({
		queryKey: ["admin-protection-page", hit.namespace_slug, hit.slug],
		queryFn: () => fetchPage(hit.slug, hit.namespace_slug),
	});
	const level: ProtectionLevel | "loading" = pageQuery.data?.protection_level ?? "loading";
	const [picked, setPicked] = useState<ProtectionLevel>("none");

	return (
		<tr className="hover:bg-neutral-50">
			<td className="px-3 py-2">
				<input
					type="checkbox"
					aria-label={`Select ${hit.title}`}
					checked={checked}
					onChange={onToggle}
				/>
			</td>
			<td className="px-3 py-2 text-sm font-medium">{hit.title}</td>
			<td className="px-3 py-2 font-mono text-xs text-neutral-500">
				{hit.namespace_slug}/{hit.slug}
			</td>
			<td className="px-3 py-2 text-xs font-mono uppercase tracking-wide">
				{level === "loading" ? "…" : level}
			</td>
			<td className="px-3 py-2">
				<select
					value={picked}
					onChange={(e) => setPicked(e.target.value as ProtectionLevel)}
					className="mr-2 rounded border border-neutral-300 bg-white px-2 py-1 text-xs"
				>
					{LEVELS.map((l) => (
						<option key={l.value} value={l.value}>
							{l.label}
						</option>
					))}
				</select>
				<button
					type="button"
					onClick={() => onProtect(picked)}
					className="rounded bg-neutral-900 px-3 py-1 text-xs font-medium text-white"
				>
					Apply
				</button>
			</td>
		</tr>
	);
}

function labelFor(level: ProtectionLevel): string {
	return LEVELS.find((l) => l.value === level)?.label ?? level;
}

function HeaderCell({ children }: { children: React.ReactNode }) {
	return (
		<th className="px-3 py-2 text-xs font-semibold uppercase tracking-wide text-neutral-500">
			{children}
		</th>
	);
}

function Skeleton() {
	return (
		<div className="space-y-3">
			{[0, 1, 2].map((i) => (
				<div key={i} className="h-10 animate-pulse rounded bg-neutral-100" />
			))}
		</div>
	);
}

function ErrorMain({ message }: { message: string }) {
	return (
		<main className="mx-auto max-w-5xl px-6 py-10">
			<h1 className="mb-4 text-2xl font-semibold">Page protection</h1>
			<ErrorBox>{message}</ErrorBox>
		</main>
	);
}

function ErrorBox({ children }: { children: React.ReactNode }) {
	return (
		<div className="rounded-md border border-red-300 bg-red-50 p-4 text-sm text-red-700">
			{children}
		</div>
	);
}
