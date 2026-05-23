//! Admin namespace manager (`/admin/namespaces`) — #47.
//!
//! Lists every namespace with its paired talk relationship. Inline create
//! + rename + delete. The create form warns when the slug looks
//! talk-prefixed (`Talk_*`) because that's reserved for auto-pairing.
//! Delete is gated by a confirmation dialog and the server enforces
//! "only when empty".

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { ConfirmDialog } from "../components/ConfirmDialog";
import {
	type ApiError,
	type AuthMePayload,
	createNamespace,
	deleteNamespace,
	fetchAuthMe,
	listNamespaces,
	type NamespaceListResponse,
	type NamespaceView,
	parsePermissions,
	updateNamespace,
} from "../lib/api";

export const Route = createFileRoute("/admin/namespaces")({
	component: NamespacesComponent,
});

function NamespacesComponent() {
	const meQuery = useQuery<AuthMePayload | null>({
		queryKey: ["auth-me"],
		queryFn: fetchAuthMe,
	});
	if (meQuery.isPending) return <Skeleton />;
	if (meQuery.isError) return <ErrorMain message={meQuery.error.message} />;
	const me = meQuery.data;
	if (!me) return <ErrorMain message="You must sign in to manage namespaces." />;
	const perms = parsePermissions(me.permissions);
	if (!perms.has("MANAGE_NAMESPACES"))
		return <ErrorMain message="You do not have the MANAGE_NAMESPACES permission." />;
	return <NamespacesPanel />;
}

function NamespacesPanel() {
	const qc = useQueryClient();
	const listQuery = useQuery<NamespaceListResponse, ApiError>({
		queryKey: ["admin-namespaces"],
		queryFn: listNamespaces,
	});

	const [creating, setCreating] = useState(false);
	const [renaming, setRenaming] = useState<NamespaceView | null>(null);
	const [confirmDelete, setConfirmDelete] = useState<NamespaceView | null>(null);

	const deleteMutation = useMutation({
		mutationFn: (slug: string) => deleteNamespace(slug),
		onSuccess: () => {
			setConfirmDelete(null);
			void qc.invalidateQueries({ queryKey: ["admin-namespaces"] });
		},
	});

	const items = listQuery.data?.items ?? [];
	const byId = new Map(items.map((ns) => [ns.id, ns]));
	const subjects = items.filter((ns) => !ns.is_talk);

	return (
		<main className="mx-auto max-w-5xl px-6 py-10">
			<header className="mb-6 flex items-end justify-between border-b border-neutral-200 pb-4">
				<div>
					<p className="font-mono text-xs uppercase tracking-wide text-neutral-500">Admin</p>
					<h1 className="mt-1 text-3xl font-semibold tracking-tight">Namespaces</h1>
					<p className="mt-2 text-sm text-neutral-600">
						Subject namespaces auto-pair with a discussion (talk) namespace on create. Delete only
						works on empty namespaces.
					</p>
				</div>
				<button
					type="button"
					onClick={() => setCreating(true)}
					className="rounded bg-neutral-900 px-4 py-2 text-sm font-medium text-white"
				>
					New namespace
				</button>
			</header>

			{listQuery.isPending && <Skeleton />}
			{listQuery.isError && (
				<ErrorBox>Failed to load namespaces: {listQuery.error.message}</ErrorBox>
			)}
			{listQuery.data && (
				<div className="overflow-x-auto rounded-md border border-neutral-200 bg-white">
					<table className="min-w-full divide-y divide-neutral-200 text-left">
						<thead>
							<tr>
								<HeaderCell>Slug</HeaderCell>
								<HeaderCell>Display name</HeaderCell>
								<HeaderCell>Talk side</HeaderCell>
								<HeaderCell>{""}</HeaderCell>
							</tr>
						</thead>
						<tbody className="divide-y divide-neutral-100">
							{subjects.map((ns) => {
								const talk = ns.paired_namespace_id ? byId.get(ns.paired_namespace_id) : null;
								return (
									<tr key={ns.id}>
										<td className="px-3 py-2 font-mono text-sm">{ns.slug}</td>
										<td className="px-3 py-2 text-sm">{ns.display_name}</td>
										<td className="px-3 py-2 font-mono text-xs text-neutral-500">
											{talk ? talk.slug : "—"}
										</td>
										<td className="px-3 py-2 text-right">
											<button
												type="button"
												onClick={() => setRenaming(ns)}
												className="mr-2 rounded border border-neutral-300 bg-white px-3 py-1 text-xs hover:bg-neutral-50"
											>
												Rename
											</button>
											<button
												type="button"
												onClick={() => setConfirmDelete(ns)}
												className="rounded border border-red-300 bg-white px-3 py-1 text-xs text-red-700 hover:bg-red-50"
											>
												Delete
											</button>
										</td>
									</tr>
								);
							})}
						</tbody>
					</table>
				</div>
			)}

			{creating && (
				<CreateForm
					onClose={() => setCreating(false)}
					onSaved={() => {
						setCreating(false);
						void qc.invalidateQueries({ queryKey: ["admin-namespaces"] });
					}}
				/>
			)}
			{renaming && (
				<RenameForm
					namespace={renaming}
					onClose={() => setRenaming(null)}
					onSaved={() => {
						setRenaming(null);
						void qc.invalidateQueries({ queryKey: ["admin-namespaces"] });
					}}
				/>
			)}

			<ConfirmDialog
				open={confirmDelete !== null}
				title="Delete namespace?"
				message={
					confirmDelete
						? `Delete the "${confirmDelete.display_name}" (${confirmDelete.slug}) namespace? This only succeeds when the namespace contains no pages — the server enforces this. Cannot be undone.`
						: ""
				}
				confirmLabel="Delete"
				busy={deleteMutation.isPending}
				onConfirm={() => {
					if (confirmDelete) {
						deleteMutation.mutate(confirmDelete.slug);
					}
				}}
				onCancel={() => setConfirmDelete(null)}
			/>
			{deleteMutation.isError && (
				<p className="mt-3 text-sm text-red-700">{(deleteMutation.error as ApiError).message}</p>
			)}
		</main>
	);
}

function CreateForm({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
	const [slug, setSlug] = useState("");
	const [displayName, setDisplayName] = useState("");
	const [error, setError] = useState<string | null>(null);

	const looksTalkPrefixed = /^talk[_:-]/i.test(slug);

	const mutation = useMutation({
		mutationFn: () => createNamespace({ slug, display_name: displayName }),
		onSuccess: onSaved,
		onError: (err: ApiError) => setError(err.message),
	});

	return (
		<Modal title="New namespace" onClose={onClose}>
			<label className="block">
				<span className="text-xs font-medium uppercase tracking-wide text-neutral-500">Slug</span>
				<input
					type="text"
					value={slug}
					onChange={(e) => setSlug(e.target.value)}
					className="mt-1 w-full rounded border border-neutral-300 px-3 py-2 font-mono text-sm"
					placeholder="Project"
				/>
				{looksTalkPrefixed && (
					<p className="mt-1 text-xs text-amber-700">
						Heads up: this slug looks talk-prefixed. The server auto-creates a
						<code className="mx-1 font-mono">Talk_*</code> namespace for every subject namespace, so
						manually naming one usually isn't what you want.
					</p>
				)}
			</label>
			<label className="mt-3 block">
				<span className="text-xs font-medium uppercase tracking-wide text-neutral-500">
					Display name
				</span>
				<input
					type="text"
					value={displayName}
					onChange={(e) => setDisplayName(e.target.value)}
					className="mt-1 w-full rounded border border-neutral-300 px-3 py-2 text-sm"
					placeholder="Project"
				/>
			</label>
			{error && <p className="mt-3 text-sm text-red-700">{error}</p>}
			<FormFooter
				onCancel={onClose}
				onSubmit={() => mutation.mutate()}
				submitLabel="Create"
				busy={mutation.isPending}
			/>
		</Modal>
	);
}

function RenameForm({
	namespace,
	onClose,
	onSaved,
}: {
	namespace: NamespaceView;
	onClose: () => void;
	onSaved: () => void;
}) {
	const [displayName, setDisplayName] = useState(namespace.display_name);
	const [error, setError] = useState<string | null>(null);
	const mutation = useMutation({
		mutationFn: () => updateNamespace(namespace.slug, { display_name: displayName }),
		onSuccess: onSaved,
		onError: (err: ApiError) => setError(err.message),
	});
	return (
		<Modal title={`Rename ${namespace.slug}`} onClose={onClose}>
			<label className="block">
				<span className="text-xs font-medium uppercase tracking-wide text-neutral-500">
					Display name
				</span>
				<input
					type="text"
					value={displayName}
					onChange={(e) => setDisplayName(e.target.value)}
					className="mt-1 w-full rounded border border-neutral-300 px-3 py-2 text-sm"
				/>
				<p className="mt-1 text-xs text-neutral-500">
					Slug renames are intentionally not supported — they would break URLs.
				</p>
			</label>
			{error && <p className="mt-3 text-sm text-red-700">{error}</p>}
			<FormFooter
				onCancel={onClose}
				onSubmit={() => mutation.mutate()}
				submitLabel="Save"
				busy={mutation.isPending}
			/>
		</Modal>
	);
}

function Modal({
	title,
	children,
	onClose,
}: {
	title: string;
	children: React.ReactNode;
	onClose: () => void;
}) {
	return (
		// biome-ignore lint/a11y/noStaticElementInteractions: temporary modal backdrop; the inner dialog content is focusable + closable from the Cancel button.
		// biome-ignore lint/a11y/useKeyWithClickEvents: companion suppression — the modal itself is keyboard-accessible through the focusable controls inside.
		<div
			className="fixed inset-0 z-40 flex items-center justify-center bg-neutral-900/40 p-4"
			onClick={(e) => {
				if (e.target === e.currentTarget) onClose();
			}}
		>
			<div className="w-[min(32rem,100%)] rounded-md border border-neutral-200 bg-white p-6 shadow-lg">
				<h2 className="mb-4 text-lg font-semibold">{title}</h2>
				{children}
			</div>
		</div>
	);
}

function FormFooter({
	onCancel,
	onSubmit,
	submitLabel,
	busy,
}: {
	onCancel: () => void;
	onSubmit: () => void;
	submitLabel: string;
	busy?: boolean;
}) {
	return (
		<div className="mt-6 flex justify-end gap-2">
			<button
				type="button"
				onClick={onCancel}
				className="rounded border border-neutral-300 bg-white px-4 py-2 text-sm hover:bg-neutral-50"
			>
				Cancel
			</button>
			<button
				type="button"
				onClick={onSubmit}
				disabled={busy}
				className="rounded bg-neutral-900 px-4 py-2 text-sm font-medium text-white disabled:opacity-50"
			>
				{submitLabel}
			</button>
		</div>
	);
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
			<h1 className="mb-4 text-2xl font-semibold">Namespaces</h1>
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
