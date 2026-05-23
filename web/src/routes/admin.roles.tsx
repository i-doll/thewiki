//! Admin role manager (`/admin/roles`) — #47.
//!
//! List of roles with their permission set surfaced as flag pills. Inline
//! create / edit / delete; delete is gated by a confirmation dialog.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { ConfirmDialog } from "../components/ConfirmDialog";
import {
	type AdminRoleListResponse,
	type AdminRoleView,
	type ApiError,
	type AuthMePayload,
	createAdminRole,
	deleteAdminRole,
	fetchAuthMe,
	listAdminRoles,
	parsePermissions,
	updateAdminRole,
} from "../lib/api";

export const Route = createFileRoute("/admin/roles")({
	component: RolesComponent,
});

const ALL_FLAGS = [
	"READ",
	"EDIT",
	"CREATE",
	"DELETE",
	"MOVE",
	"PROTECT",
	"MANAGE_USERS",
	"MANAGE_ROLES",
	"MANAGE_NAMESPACES",
	"VIEW_AUDIT_LOG",
	"MANAGE_BLOCKLIST",
	"REVIEW_EDITS",
] as const;

function RolesComponent() {
	const meQuery = useQuery<AuthMePayload | null>({
		queryKey: ["auth-me"],
		queryFn: fetchAuthMe,
	});
	if (meQuery.isPending) return <Skeleton />;
	if (meQuery.isError) return <ErrorMain message={meQuery.error.message} />;
	const me = meQuery.data;
	if (!me) return <ErrorMain message="You must sign in to manage roles." />;
	const perms = parsePermissions(me.permissions);
	if (!perms.has("MANAGE_ROLES"))
		return <ErrorMain message="You do not have the MANAGE_ROLES permission." />;

	return <RolesPanel />;
}

function RolesPanel() {
	const qc = useQueryClient();
	const rolesQuery = useQuery<AdminRoleListResponse, ApiError>({
		queryKey: ["admin-roles"],
		queryFn: listAdminRoles,
	});
	const [editing, setEditing] = useState<AdminRoleView | null>(null);
	const [creating, setCreating] = useState(false);
	const [confirmDelete, setConfirmDelete] = useState<AdminRoleView | null>(null);

	const deleteMutation = useMutation({
		mutationFn: (id: string) => deleteAdminRole(id),
		onSuccess: () => {
			setConfirmDelete(null);
			void qc.invalidateQueries({ queryKey: ["admin-roles"] });
		},
	});

	return (
		<main className="mx-auto max-w-5xl px-6 py-10">
			<header className="mb-6 flex items-end justify-between border-b border-neutral-200 pb-4">
				<div>
					<p className="font-mono text-xs uppercase tracking-wide text-neutral-500">Admin</p>
					<h1 className="mt-1 text-3xl font-semibold tracking-tight">Roles</h1>
					<p className="mt-2 text-sm text-neutral-600">
						Roles bundle permission flags. Click a row to rename or change its flag set.
					</p>
				</div>
				<button
					type="button"
					onClick={() => setCreating(true)}
					className="rounded bg-neutral-900 px-4 py-2 text-sm font-medium text-white"
				>
					New role
				</button>
			</header>

			{rolesQuery.isPending && <Skeleton />}
			{rolesQuery.isError && <ErrorBox>Failed to load roles: {rolesQuery.error.message}</ErrorBox>}
			{rolesQuery.data && (
				<div className="space-y-3">
					{rolesQuery.data.items.map((role) => (
						<div key={role.id} className="rounded-md border border-neutral-200 bg-white p-4">
							<div className="flex items-start justify-between gap-4">
								<div>
									<h2 className="text-base font-semibold">{role.display_name}</h2>
									<p className="font-mono text-xs text-neutral-500">{role.name}</p>
									<p className="mt-1 text-xs text-neutral-500">
										{role.assigned_users} user{role.assigned_users === 1 ? "" : "s"} assigned
									</p>
								</div>
								<div className="flex gap-2">
									<button
										type="button"
										onClick={() => setEditing(role)}
										className="rounded border border-neutral-300 bg-white px-3 py-1 text-xs hover:bg-neutral-50"
									>
										Edit
									</button>
									<button
										type="button"
										onClick={() => setConfirmDelete(role)}
										className="rounded border border-red-300 bg-white px-3 py-1 text-xs text-red-700 hover:bg-red-50"
									>
										Delete
									</button>
								</div>
							</div>
							<div className="mt-3 flex flex-wrap gap-1">
								{role.permission_flags.map((flag) => (
									<span
										key={flag}
										className="rounded bg-neutral-200 px-2 py-0.5 text-[10px] font-mono uppercase tracking-wide"
									>
										{flag}
									</span>
								))}
								{role.permission_flags.length === 0 && (
									<em className="text-xs text-neutral-400">no flags</em>
								)}
							</div>
						</div>
					))}
					{rolesQuery.data.items.length === 0 && (
						<p className="text-sm italic text-neutral-500">No roles defined.</p>
					)}
				</div>
			)}

			{creating && (
				<RoleForm
					mode="create"
					initial={null}
					onClose={() => setCreating(false)}
					onSaved={() => {
						setCreating(false);
						void qc.invalidateQueries({ queryKey: ["admin-roles"] });
					}}
				/>
			)}
			{editing && (
				<RoleForm
					mode="edit"
					initial={editing}
					onClose={() => setEditing(null)}
					onSaved={() => {
						setEditing(null);
						void qc.invalidateQueries({ queryKey: ["admin-roles"] });
					}}
				/>
			)}

			<ConfirmDialog
				open={confirmDelete !== null}
				title="Delete role?"
				message={
					confirmDelete
						? confirmDelete.assigned_users > 0
							? `Cannot delete "${confirmDelete.display_name}" — it is still assigned to ${confirmDelete.assigned_users} user(s). Remove those assignments first.`
							: `Delete the "${confirmDelete.display_name}" role permanently? This cannot be undone.`
						: ""
				}
				confirmLabel="Delete"
				busy={deleteMutation.isPending}
				onConfirm={() => {
					if (confirmDelete && confirmDelete.assigned_users === 0) {
						deleteMutation.mutate(confirmDelete.id);
					} else {
						setConfirmDelete(null);
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

function RoleForm({
	mode,
	initial,
	onClose,
	onSaved,
}: {
	mode: "create" | "edit";
	initial: AdminRoleView | null;
	onClose: () => void;
	onSaved: () => void;
}) {
	const [name, setName] = useState(initial?.name ?? "");
	const [displayName, setDisplayName] = useState(initial?.display_name ?? "");
	const [flags, setFlags] = useState<Set<string>>(new Set(initial?.permission_flags ?? []));
	const [error, setError] = useState<string | null>(null);

	const saveMutation = useMutation({
		mutationFn: async () => {
			if (mode === "create") {
				await createAdminRole({
					name,
					display_name: displayName,
					permissions: [...flags],
				});
			} else if (initial) {
				await updateAdminRole(initial.id, {
					display_name: displayName,
					permissions: [...flags],
				});
			}
		},
		onSuccess: onSaved,
		onError: (err: ApiError) => setError(err.message),
	});

	return (
		<div className="fixed inset-0 z-40 flex items-center justify-center bg-neutral-900/40 p-4">
			<div className="w-[min(40rem,100%)] rounded-md border border-neutral-200 bg-white p-6 shadow-lg">
				<h2 className="mb-4 text-lg font-semibold">
					{mode === "create" ? "Create role" : `Edit ${initial?.display_name}`}
				</h2>
				<div className="space-y-3">
					{mode === "create" && (
						<label className="block">
							<span className="text-xs font-medium uppercase tracking-wide text-neutral-500">
								Machine name
							</span>
							<input
								type="text"
								value={name}
								onChange={(e) => setName(e.target.value)}
								className="mt-1 w-full rounded border border-neutral-300 px-3 py-2 font-mono text-sm"
								placeholder="moderator"
							/>
						</label>
					)}
					<label className="block">
						<span className="text-xs font-medium uppercase tracking-wide text-neutral-500">
							Display name
						</span>
						<input
							type="text"
							value={displayName}
							onChange={(e) => setDisplayName(e.target.value)}
							className="mt-1 w-full rounded border border-neutral-300 px-3 py-2 text-sm"
							placeholder="Moderator"
						/>
					</label>
					<div>
						<span className="text-xs font-medium uppercase tracking-wide text-neutral-500">
							Permissions
						</span>
						<div className="mt-1 grid grid-cols-2 gap-2 sm:grid-cols-3">
							{ALL_FLAGS.map((flag) => {
								const checked = flags.has(flag);
								return (
									<label
										key={flag}
										className="flex items-center gap-2 rounded border border-neutral-200 px-2 py-1 text-xs"
									>
										<input
											type="checkbox"
											checked={checked}
											onChange={() => {
												const next = new Set(flags);
												if (checked) next.delete(flag);
												else next.add(flag);
												setFlags(next);
											}}
										/>
										<span className="font-mono">{flag}</span>
									</label>
								);
							})}
						</div>
					</div>
				</div>
				{error && <p className="mt-3 text-sm text-red-700">{error}</p>}
				<div className="mt-6 flex justify-end gap-2">
					<button
						type="button"
						onClick={onClose}
						className="rounded border border-neutral-300 bg-white px-4 py-2 text-sm hover:bg-neutral-50"
					>
						Cancel
					</button>
					<button
						type="button"
						onClick={() => saveMutation.mutate()}
						disabled={saveMutation.isPending}
						className="rounded bg-neutral-900 px-4 py-2 text-sm font-medium text-white disabled:opacity-50"
					>
						{mode === "create" ? "Create" : "Save"}
					</button>
				</div>
			</div>
		</div>
	);
}

function Skeleton() {
	return (
		<div className="space-y-3">
			{[0, 1, 2].map((i) => (
				<div key={i} className="h-20 animate-pulse rounded bg-neutral-100" />
			))}
		</div>
	);
}

function ErrorMain({ message }: { message: string }) {
	return (
		<main className="mx-auto max-w-5xl px-6 py-10">
			<h1 className="mb-4 text-2xl font-semibold">Roles</h1>
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
