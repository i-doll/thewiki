//! Audit log viewer (`/admin/audit-log`) — #47.
//!
//! Wraps the existing `/api/v1/audit-log` endpoint. Filterable by actor,
//! action, target kind, and date range. Linkable Atom feed for ops
//! integrations.

import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import {
	type ApiError,
	AUDIT_LOG_ATOM_URL,
	type AuditLogQuery as AuditFilters,
	type AuditLogEntryView,
	type AuditLogListResponse,
	type AuthMePayload,
	fetchAuthMe,
	listAuditLog,
	parsePermissions,
} from "../lib/api";

export const Route = createFileRoute("/admin/audit-log")({
	component: AuditLogComponent,
});

function AuditLogComponent() {
	const meQuery = useQuery<AuthMePayload | null>({
		queryKey: ["auth-me"],
		queryFn: fetchAuthMe,
	});
	if (meQuery.isPending) return <Skeleton />;
	if (meQuery.isError) return <ErrorMain message={meQuery.error.message} />;
	const me = meQuery.data;
	if (!me) return <ErrorMain message="You must sign in to view the audit log." />;
	const perms = parsePermissions(me.permissions);
	if (!perms.has("VIEW_AUDIT_LOG"))
		return <ErrorMain message="You do not have the VIEW_AUDIT_LOG permission." />;
	return <AuditLogPanel />;
}

function AuditLogPanel() {
	const [filters, setFilters] = useState<AuditFilters>({});
	const [appliedFilters, setAppliedFilters] = useState<AuditFilters>({});
	const [targetKind, setTargetKind] = useState<string>("");

	const query = useQuery<AuditLogListResponse, ApiError>({
		queryKey: ["admin-audit-log", appliedFilters],
		queryFn: () => listAuditLog({ ...appliedFilters, limit: 100 }),
	});

	const apply = () => {
		setAppliedFilters({ ...filters });
	};
	const clear = () => {
		setFilters({});
		setTargetKind("");
		setAppliedFilters({});
	};

	const rows = (query.data?.items ?? []).filter((entry) => {
		if (!targetKind) return true;
		return entry.target_kind === targetKind;
	});

	return (
		<main className="mx-auto max-w-6xl px-6 py-10">
			<header className="mb-6 flex items-end justify-between border-b border-neutral-200 pb-4">
				<div>
					<p className="font-mono text-xs uppercase tracking-wide text-neutral-500">Admin</p>
					<h1 className="mt-1 text-3xl font-semibold tracking-tight">Audit log</h1>
					<p className="mt-2 text-sm text-neutral-600">
						Every administrative action lands here. Filter by actor, action, target kind, or date
						range.
					</p>
				</div>
				<a
					href={AUDIT_LOG_ATOM_URL}
					target="_blank"
					rel="noreferrer"
					className="rounded border border-neutral-300 bg-white px-3 py-1.5 text-xs hover:bg-neutral-50"
				>
					Atom feed
				</a>
			</header>

			<section className="mb-4 rounded-md border border-neutral-200 bg-neutral-50 p-4">
				<div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-5">
					<input
						type="text"
						placeholder="Actor username"
						value={filters.actor ?? ""}
						onChange={(e) => setFilters({ ...filters, actor: e.target.value })}
						className="rounded border border-neutral-300 px-3 py-2 text-sm"
					/>
					<input
						type="text"
						placeholder="Action (e.g. page.create)"
						value={filters.action ?? ""}
						onChange={(e) => setFilters({ ...filters, action: e.target.value })}
						className="rounded border border-neutral-300 px-3 py-2 text-sm"
					/>
					<input
						type="text"
						placeholder="Target kind"
						value={targetKind}
						onChange={(e) => setTargetKind(e.target.value)}
						className="rounded border border-neutral-300 px-3 py-2 text-sm"
					/>
					<input
						type="datetime-local"
						value={filters.since ?? ""}
						onChange={(e) => {
							const next = { ...filters };
							const v = toRfc3339(e.target.value);
							if (v) next.since = v;
							else delete next.since;
							setFilters(next);
						}}
						className="rounded border border-neutral-300 px-3 py-2 text-sm"
					/>
					<input
						type="datetime-local"
						value={filters.until ?? ""}
						onChange={(e) => {
							const next = { ...filters };
							const v = toRfc3339(e.target.value);
							if (v) next.until = v;
							else delete next.until;
							setFilters(next);
						}}
						className="rounded border border-neutral-300 px-3 py-2 text-sm"
					/>
				</div>
				<div className="mt-3 flex justify-end gap-2">
					<button
						type="button"
						onClick={clear}
						className="rounded border border-neutral-300 bg-white px-3 py-1.5 text-xs hover:bg-neutral-100"
					>
						Clear
					</button>
					<button
						type="button"
						onClick={apply}
						className="rounded bg-neutral-900 px-3 py-1.5 text-xs font-medium text-white"
					>
						Apply
					</button>
				</div>
			</section>

			{query.isPending && <Skeleton />}
			{query.isError && <ErrorBox>Failed to load audit log: {query.error.message}</ErrorBox>}
			{query.data && (
				<div className="overflow-x-auto rounded-md border border-neutral-200 bg-white">
					<table className="min-w-full divide-y divide-neutral-200 text-left">
						<thead>
							<tr>
								<HeaderCell>When</HeaderCell>
								<HeaderCell>Actor</HeaderCell>
								<HeaderCell>Action</HeaderCell>
								<HeaderCell>Target</HeaderCell>
								<HeaderCell>Metadata</HeaderCell>
							</tr>
						</thead>
						<tbody className="divide-y divide-neutral-100">
							{rows.map((entry) => (
								<AuditRow key={entry.id} entry={entry} />
							))}
							{rows.length === 0 && (
								<tr>
									<td colSpan={5} className="px-3 py-6 text-center text-sm text-neutral-500">
										No audit entries match.
									</td>
								</tr>
							)}
						</tbody>
					</table>
				</div>
			)}
		</main>
	);
}

function AuditRow({ entry }: { entry: AuditLogEntryView }) {
	const [expanded, setExpanded] = useState(false);
	return (
		<tr className="align-top hover:bg-neutral-50">
			<td className="whitespace-nowrap px-3 py-2 text-xs text-neutral-500">
				{new Date(entry.created_at).toLocaleString()}
			</td>
			<td className="px-3 py-2 font-mono text-xs">{entry.actor_username}</td>
			<td className="px-3 py-2 font-mono text-xs">{entry.action}</td>
			<td className="px-3 py-2 text-sm">
				<div className="font-mono text-[10px] uppercase tracking-wide text-neutral-500">
					{entry.target_kind}
				</div>
				<div>{entry.target_label ?? entry.target_id}</div>
			</td>
			<td className="px-3 py-2">
				<button
					type="button"
					onClick={() => setExpanded((v) => !v)}
					className="text-xs text-blue-700 hover:underline"
				>
					{expanded ? "Hide" : "Show"}
				</button>
				{expanded && (
					<pre className="mt-2 max-w-md overflow-x-auto rounded bg-neutral-100 p-2 text-[11px] leading-snug">
						{JSON.stringify(entry.metadata, null, 2)}
					</pre>
				)}
			</td>
		</tr>
	);
}

function toRfc3339(local: string): string | undefined {
	if (!local) return undefined;
	// The browser's datetime-local control returns `YYYY-MM-DDTHH:mm` in
	// local time. Pass it through Date so we get a proper UTC RFC 3339
	// stamp the server's `parse_rfc3339` accepts.
	const d = new Date(local);
	if (Number.isNaN(d.getTime())) return undefined;
	return d.toISOString();
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
			{[0, 1, 2, 3].map((i) => (
				<div key={i} className="h-10 animate-pulse rounded bg-neutral-100" />
			))}
		</div>
	);
}

function ErrorMain({ message }: { message: string }) {
	return (
		<main className="mx-auto max-w-6xl px-6 py-10">
			<h1 className="mb-4 text-2xl font-semibold">Audit log</h1>
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
