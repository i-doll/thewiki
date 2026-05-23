//! Admin UI for the IP / URL blocklists (#42).
//!
//! Mounted at `/admin/blocklists`. The broader admin UI (#47) will hang
//! more sub-routes off `/admin/*` so this file deliberately keeps the
//! markup self-contained and the page header neutral.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import {
	ApiError,
	createIpBlocklistEntry,
	createUrlBlocklistEntry,
	deleteIpBlocklistEntry,
	deleteUrlBlocklistEntry,
	fetchAuthMe,
	type IpBlocklistEntry,
	type IpBlocklistListResponse,
	listIpBlocklist,
	listUrlBlocklist,
	parsePermissions,
	type UrlBlocklistEntry,
	type UrlBlocklistListResponse,
} from "../lib/api";

export const Route = createFileRoute("/admin/blocklists")({
	component: BlocklistsComponent,
});

type Tab = "ip" | "url";

function BlocklistsComponent() {
	const [tab, setTab] = useState<Tab>("ip");
	const meQuery = useQuery({
		queryKey: ["auth-me"],
		queryFn: fetchAuthMe,
	});

	if (meQuery.isPending) {
		return <Skeleton />;
	}
	if (meQuery.isError) {
		return (
			<ErrorBox>Failed to load session: {meQuery.error.message}</ErrorBox>
		);
	}

	const me = meQuery.data;
	if (!me) {
		return (
			<main className="mx-auto max-w-4xl px-6 py-10">
				<h1 className="mb-4 text-2xl font-semibold">Blocklists</h1>
				<ErrorBox>You must sign in to manage blocklists.</ErrorBox>
			</main>
		);
	}
	const perms = parsePermissions(me.permissions);
	if (!perms.has("MANAGE_BLOCKLIST")) {
		return (
			<main className="mx-auto max-w-4xl px-6 py-10">
				<h1 className="mb-4 text-2xl font-semibold">Blocklists</h1>
				<ErrorBox>
					You do not have the <code>MANAGE_BLOCKLIST</code> permission.
				</ErrorBox>
			</main>
		);
	}

	return (
		<main className="mx-auto max-w-4xl px-6 py-10">
			<header className="mb-6 border-b border-neutral-200 pb-4">
				<p className="font-mono text-xs uppercase tracking-wide text-neutral-500">
					Admin
				</p>
				<h1 className="mt-1 text-3xl font-semibold tracking-tight">Blocklists</h1>
				<p className="mt-2 text-sm text-neutral-600">
					IPs and URL patterns operators have flagged for this wiki.
				</p>
			</header>

			<nav className="mb-6 flex gap-2 border-b border-neutral-200">
				<TabButton active={tab === "ip"} onClick={() => setTab("ip")}>
					IP CIDRs
				</TabButton>
				<TabButton active={tab === "url"} onClick={() => setTab("url")}>
					URL patterns
				</TabButton>
			</nav>

			{tab === "ip" ? <IpPanel /> : <UrlPanel />}
		</main>
	);
}

function IpPanel() {
	const qc = useQueryClient();
	const [cidr, setCidr] = useState("");
	const [reason, setReason] = useState("");
	const [formError, setFormError] = useState<string | null>(null);

	const listQuery = useQuery<IpBlocklistListResponse, ApiError>({
		queryKey: ["admin-blocklist-ip"],
		queryFn: listIpBlocklist,
	});

	const createMutation = useMutation({
		mutationFn: () => createIpBlocklistEntry({ cidr, reason }),
		onSuccess: () => {
			setCidr("");
			setReason("");
			setFormError(null);
			void qc.invalidateQueries({ queryKey: ["admin-blocklist-ip"] });
		},
		onError: (err: ApiError) => setFormError(err.message),
	});

	const deleteMutation = useMutation({
		mutationFn: (id: string) => deleteIpBlocklistEntry(id),
		onSuccess: () => {
			void qc.invalidateQueries({ queryKey: ["admin-blocklist-ip"] });
		},
	});

	const handleDelete = (entry: IpBlocklistEntry) => {
		const ok = window.confirm(
			`Remove ${entry.cidr} from the IP blocklist?`,
		);
		if (ok) {
			deleteMutation.mutate(entry.id);
		}
	};

	return (
		<section>
			<form
				className="mb-6 rounded-md border border-neutral-200 bg-neutral-50 p-4"
				onSubmit={(e) => {
					e.preventDefault();
					createMutation.mutate();
				}}
			>
				<h2 className="mb-3 text-sm font-semibold uppercase tracking-wide text-neutral-700">
					Add CIDR
				</h2>
				<div className="grid gap-3 sm:grid-cols-[1fr_2fr_auto]">
					<input
						required
						type="text"
						value={cidr}
						onChange={(e) => setCidr(e.target.value)}
						placeholder="203.0.113.0/24"
						className="rounded border border-neutral-300 px-3 py-2 font-mono text-sm"
					/>
					<input
						type="text"
						value={reason}
						onChange={(e) => setReason(e.target.value)}
						placeholder="Reason (optional)"
						className="rounded border border-neutral-300 px-3 py-2 text-sm"
					/>
					<button
						type="submit"
						disabled={createMutation.isPending}
						className="rounded bg-neutral-900 px-4 py-2 text-sm font-medium text-white disabled:opacity-50"
					>
						Add
					</button>
				</div>
				{formError && (
					<p className="mt-2 text-sm text-red-700">{formError}</p>
				)}
			</form>

			{listQuery.isPending && <Skeleton />}
			{listQuery.isError && (
				<ErrorBox>Failed to load: {listQuery.error.message}</ErrorBox>
			)}
			{listQuery.data && (
				<EntryTable
					columns={["CIDR", "Reason", "Created", "Created by", ""]}
					rows={listQuery.data.items.map((entry) => ({
						id: entry.id,
						cells: [
							<code key="cidr" className="font-mono text-sm">
								{entry.cidr}
							</code>,
							<span key="r" className="text-sm text-neutral-700">
								{entry.reason || <em className="text-neutral-400">—</em>}
							</span>,
							<time key="t" className="text-xs text-neutral-500">
								{new Date(entry.created_at).toLocaleString()}
							</time>,
							<span key="c" className="font-mono text-xs text-neutral-500">
								{entry.created_by.slice(0, 8)}
							</span>,
							<button
								key="d"
								type="button"
								onClick={() => handleDelete(entry)}
								disabled={deleteMutation.isPending}
								className="rounded border border-red-300 px-3 py-1 text-xs text-red-700 hover:bg-red-50 disabled:opacity-50"
							>
								Remove
							</button>,
						],
					}))}
					empty="No IPs blocked."
				/>
			)}
		</section>
	);
}

function UrlPanel() {
	const qc = useQueryClient();
	const [pattern, setPattern] = useState("");
	const [reason, setReason] = useState("");
	const [formError, setFormError] = useState<string | null>(null);

	const listQuery = useQuery<UrlBlocklistListResponse, ApiError>({
		queryKey: ["admin-blocklist-url"],
		queryFn: listUrlBlocklist,
	});

	const createMutation = useMutation({
		mutationFn: () => createUrlBlocklistEntry({ pattern, reason }),
		onSuccess: () => {
			setPattern("");
			setReason("");
			setFormError(null);
			void qc.invalidateQueries({ queryKey: ["admin-blocklist-url"] });
		},
		onError: (err: ApiError) => setFormError(err.message),
	});

	const deleteMutation = useMutation({
		mutationFn: (id: string) => deleteUrlBlocklistEntry(id),
		onSuccess: () => {
			void qc.invalidateQueries({ queryKey: ["admin-blocklist-url"] });
		},
	});

	const handleDelete = (entry: UrlBlocklistEntry) => {
		const ok = window.confirm(
			`Remove ${entry.pattern} from the URL blocklist?`,
		);
		if (ok) {
			deleteMutation.mutate(entry.id);
		}
	};

	return (
		<section>
			<form
				className="mb-6 rounded-md border border-neutral-200 bg-neutral-50 p-4"
				onSubmit={(e) => {
					e.preventDefault();
					createMutation.mutate();
				}}
			>
				<h2 className="mb-3 text-sm font-semibold uppercase tracking-wide text-neutral-700">
					Add pattern
				</h2>
				<p className="mb-3 text-xs text-neutral-500">
					Rust <code>regex</code> syntax. Use <code>(?i)</code> for
					case-insensitive matches.
				</p>
				<div className="grid gap-3 sm:grid-cols-[1fr_2fr_auto]">
					<input
						required
						type="text"
						value={pattern}
						onChange={(e) => setPattern(e.target.value)}
						placeholder={String.raw`\bevil\.example\b`}
						className="rounded border border-neutral-300 px-3 py-2 font-mono text-sm"
					/>
					<input
						type="text"
						value={reason}
						onChange={(e) => setReason(e.target.value)}
						placeholder="Reason (optional)"
						className="rounded border border-neutral-300 px-3 py-2 text-sm"
					/>
					<button
						type="submit"
						disabled={createMutation.isPending}
						className="rounded bg-neutral-900 px-4 py-2 text-sm font-medium text-white disabled:opacity-50"
					>
						Add
					</button>
				</div>
				{formError && (
					<p className="mt-2 text-sm text-red-700">{formError}</p>
				)}
			</form>

			{listQuery.isPending && <Skeleton />}
			{listQuery.isError && (
				<ErrorBox>Failed to load: {listQuery.error.message}</ErrorBox>
			)}
			{listQuery.data && (
				<EntryTable
					columns={["Pattern", "Reason", "Created", "Created by", ""]}
					rows={listQuery.data.items.map((entry) => ({
						id: entry.id,
						cells: [
							<code key="p" className="font-mono text-sm">
								{entry.pattern}
							</code>,
							<span key="r" className="text-sm text-neutral-700">
								{entry.reason || <em className="text-neutral-400">—</em>}
							</span>,
							<time key="t" className="text-xs text-neutral-500">
								{new Date(entry.created_at).toLocaleString()}
							</time>,
							<span key="c" className="font-mono text-xs text-neutral-500">
								{entry.created_by.slice(0, 8)}
							</span>,
							<button
								key="d"
								type="button"
								onClick={() => handleDelete(entry)}
								disabled={deleteMutation.isPending}
								className="rounded border border-red-300 px-3 py-1 text-xs text-red-700 hover:bg-red-50 disabled:opacity-50"
							>
								Remove
							</button>,
						],
					}))}
					empty="No URL patterns blocked."
				/>
			)}
		</section>
	);
}

function TabButton({
	active,
	onClick,
	children,
}: {
	active: boolean;
	onClick: () => void;
	children: React.ReactNode;
}) {
	return (
		<button
			type="button"
			onClick={onClick}
			className={
				active
					? "border-b-2 border-neutral-900 px-3 py-2 text-sm font-medium text-neutral-900"
					: "px-3 py-2 text-sm text-neutral-500 hover:text-neutral-700"
			}
		>
			{children}
		</button>
	);
}

interface Row {
	id: string;
	cells: React.ReactNode[];
}

function EntryTable({
	columns,
	rows,
	empty,
}: {
	columns: string[];
	rows: Row[];
	empty: string;
}) {
	if (rows.length === 0) {
		return <p className="text-sm italic text-neutral-500">{empty}</p>;
	}
	return (
		<div className="overflow-x-auto">
			<table className="min-w-full divide-y divide-neutral-200 text-left">
				<thead>
					<tr>
						{columns.map((col) => (
							<th
								key={col}
								className="px-3 py-2 text-xs font-semibold uppercase tracking-wide text-neutral-500"
							>
								{col}
							</th>
						))}
					</tr>
				</thead>
				<tbody className="divide-y divide-neutral-100">
					{rows.map((row) => (
						<tr key={row.id}>
							{row.cells.map((cell, idx) => (
								<td key={`${row.id}-${idx}`} className="px-3 py-2 align-top">
									{cell}
								</td>
							))}
						</tr>
					))}
				</tbody>
			</table>
		</div>
	);
}

function Skeleton() {
	return (
		<div className="space-y-3">
			<div className="h-8 w-2/3 animate-pulse rounded bg-neutral-200" />
			<div className="h-8 w-1/2 animate-pulse rounded bg-neutral-200" />
		</div>
	);
}

function ErrorBox({ children }: { children: React.ReactNode }) {
	return (
		<div className="rounded-md border border-red-300 bg-red-50 p-4 text-sm text-red-700">
			{children}
		</div>
	);
}
