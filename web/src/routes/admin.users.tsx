//! Admin user list (`/admin/users`) — #47.
//!
//! Search + role-filter, paginated. Multi-select rows enable a bulk
//! "assign / remove role" toolbar. Each user row links to the detail page
//! at `/admin/users/{id}` for individual role management.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { useMemo, useState } from "react";
import { ConfirmDialog } from "../components/ConfirmDialog";
import {
	type AdminRoleListResponse,
	type AdminUserListResponse,
	type AdminUserView,
	type ApiError,
	type AuthMePayload,
	assignUserRoles,
	fetchAuthMe,
	listAdminRoles,
	listAdminUsers,
	parsePermissions,
	revokeUserRole,
} from "../lib/api";

export const Route = createFileRoute("/admin/users")({
	component: AdminUsersComponent,
	// Accept `?role=<uuid>` so other admin pages can deep-link in with a
	// role filter pre-applied (most notably the role-delete 409 dialog
	// surfacing the affected users). Unknown keys are dropped — TanStack
	// Router throws otherwise.
	validateSearch: (search: Record<string, unknown>) => {
		const role = typeof search.role === "string" ? search.role : "";
		return { role };
	},
});

function AdminUsersComponent() {
	const meQuery = useQuery<AuthMePayload | null>({
		queryKey: ["auth-me"],
		queryFn: fetchAuthMe,
	});
	if (meQuery.isPending) return <Skeleton />;
	if (meQuery.isError)
		return <ErrorMain message={`Failed to load session: ${meQuery.error.message}`} />;
	const me = meQuery.data;
	if (!me) return <ErrorMain message="You must sign in to manage users." />;
	const perms = parsePermissions(me.permissions);
	if (!perms.has("MANAGE_USERS"))
		return <ErrorMain message="You do not have the MANAGE_USERS permission." />;

	return <UsersPanel />;
}

function UsersPanel() {
	const qc = useQueryClient();
	// Seed the role filter from `?role=` so role-delete 409 dialogs can
	// deep-link admins to the list of assigned users.
	const { role: initialRoleFilter } = Route.useSearch();
	const [search, setSearch] = useState("");
	const [roleFilter, setRoleFilter] = useState<string>(initialRoleFilter);
	const [selected, setSelected] = useState<Set<string>>(new Set());
	const [bulkRoleId, setBulkRoleId] = useState<string>("");
	const [confirm, setConfirm] = useState<
		| { kind: "assign"; users: AdminUserView[]; roleId: string; roleName: string }
		| { kind: "remove"; users: AdminUserView[]; roleId: string; roleName: string }
		| null
	>(null);

	const usersQuery = useQuery<AdminUserListResponse, ApiError>({
		queryKey: ["admin-users", search, roleFilter],
		queryFn: () => {
			const opts: {
				search?: string;
				role_id?: string;
				limit?: number;
			} = { limit: 100 };
			if (search) opts.search = search;
			if (roleFilter) opts.role_id = roleFilter;
			return listAdminUsers(opts);
		},
	});
	const rolesQuery = useQuery<AdminRoleListResponse, ApiError>({
		queryKey: ["admin-roles"],
		queryFn: listAdminRoles,
	});

	const assignMutation = useMutation({
		mutationFn: async (args: { userIds: string[]; roleId: string }) => {
			for (const uid of args.userIds) {
				await assignUserRoles(uid, { role_ids: [args.roleId] });
			}
		},
		onSuccess: () => {
			setSelected(new Set());
			void qc.invalidateQueries({ queryKey: ["admin-users"] });
			void qc.invalidateQueries({ queryKey: ["admin-roles"] });
		},
	});

	const revokeMutation = useMutation({
		mutationFn: async (args: { userIds: string[]; roleId: string }) => {
			for (const uid of args.userIds) {
				await revokeUserRole(uid, args.roleId);
			}
		},
		onSuccess: () => {
			setSelected(new Set());
			void qc.invalidateQueries({ queryKey: ["admin-users"] });
			void qc.invalidateQueries({ queryKey: ["admin-roles"] });
		},
	});

	const users = usersQuery.data?.items ?? [];
	const roles = rolesQuery.data?.items ?? [];

	const selectedUsers = useMemo(() => users.filter((u) => selected.has(u.id)), [users, selected]);

	const toggleAll = () => {
		if (selected.size === users.length) {
			setSelected(new Set());
		} else {
			setSelected(new Set(users.map((u) => u.id)));
		}
	};
	const toggleOne = (id: string) => {
		const next = new Set(selected);
		if (next.has(id)) next.delete(id);
		else next.add(id);
		setSelected(next);
	};

	const handleBulkAssign = () => {
		if (!bulkRoleId || selected.size === 0) return;
		const role = roles.find((r) => r.id === bulkRoleId);
		if (!role) return;
		setConfirm({
			kind: "assign",
			users: selectedUsers,
			roleId: role.id,
			roleName: role.display_name,
		});
	};
	const handleBulkRemove = () => {
		if (!bulkRoleId || selected.size === 0) return;
		const role = roles.find((r) => r.id === bulkRoleId);
		if (!role) return;
		setConfirm({
			kind: "remove",
			users: selectedUsers,
			roleId: role.id,
			roleName: role.display_name,
		});
	};

	return (
		<main className="mx-auto max-w-5xl px-6 py-10">
			<Header />

			<section className="mb-4 rounded-md border border-neutral-200 bg-neutral-50 p-4">
				<div className="grid gap-3 sm:grid-cols-[1fr_1fr_auto]">
					<input
						type="search"
						value={search}
						onChange={(e) => setSearch(e.target.value)}
						placeholder="Search by username or email"
						className="rounded border border-neutral-300 px-3 py-2 text-sm"
					/>
					<select
						value={roleFilter}
						onChange={(e) => setRoleFilter(e.target.value)}
						className="rounded border border-neutral-300 px-3 py-2 text-sm"
					>
						<option value="">All roles</option>
						{roles.map((r) => (
							<option key={r.id} value={r.id}>
								{r.display_name}
							</option>
						))}
					</select>
					<button
						type="button"
						onClick={() => {
							setSearch("");
							setRoleFilter("");
						}}
						className="rounded border border-neutral-300 bg-white px-3 py-2 text-sm hover:bg-neutral-100"
					>
						Clear
					</button>
				</div>
			</section>

			{selected.size > 0 && (
				<section className="mb-4 flex items-center gap-3 rounded-md border border-neutral-200 bg-amber-50 px-4 py-3 text-sm">
					<span>
						<strong>{selected.size}</strong> user{selected.size === 1 ? "" : "s"} selected
					</span>
					<select
						value={bulkRoleId}
						onChange={(e) => setBulkRoleId(e.target.value)}
						className="rounded border border-neutral-300 bg-white px-2 py-1 text-sm"
					>
						<option value="">— pick role —</option>
						{roles.map((r) => (
							<option key={r.id} value={r.id}>
								{r.display_name}
							</option>
						))}
					</select>
					<button
						type="button"
						onClick={handleBulkAssign}
						disabled={!bulkRoleId}
						className="rounded bg-neutral-900 px-3 py-1 text-xs font-medium text-white disabled:opacity-50"
					>
						Assign
					</button>
					<button
						type="button"
						onClick={handleBulkRemove}
						disabled={!bulkRoleId}
						className="rounded border border-red-300 bg-white px-3 py-1 text-xs text-red-700 hover:bg-red-50 disabled:opacity-50"
					>
						Remove
					</button>
				</section>
			)}

			{usersQuery.isPending && <Skeleton />}
			{usersQuery.isError && <ErrorBox>Failed to load users: {usersQuery.error.message}</ErrorBox>}
			{usersQuery.data && (
				<div className="overflow-x-auto rounded-md border border-neutral-200 bg-white">
					<table className="min-w-full divide-y divide-neutral-200 text-left">
						<thead>
							<tr>
								<th className="w-10 px-3 py-2">
									<input
										type="checkbox"
										aria-label="Select all"
										checked={users.length > 0 && selected.size === users.length}
										onChange={toggleAll}
									/>
								</th>
								<HeaderCell>Username</HeaderCell>
								<HeaderCell>Display name</HeaderCell>
								<HeaderCell>Email</HeaderCell>
								<HeaderCell>Roles</HeaderCell>
								<HeaderCell>Created</HeaderCell>
								<HeaderCell>{""}</HeaderCell>
							</tr>
						</thead>
						<tbody className="divide-y divide-neutral-100">
							{users.map((u) => (
								<tr key={u.id} className="hover:bg-neutral-50">
									<td className="px-3 py-2">
										<input
											type="checkbox"
											aria-label={`Select ${u.username}`}
											checked={selected.has(u.id)}
											onChange={() => toggleOne(u.id)}
										/>
									</td>
									<td className="px-3 py-2 font-mono text-sm">{u.username}</td>
									<td className="px-3 py-2 text-sm">{u.display_name ?? "—"}</td>
									<td className="px-3 py-2 text-sm text-neutral-600">{u.email ?? "—"}</td>
									<td className="px-3 py-2 text-sm">
										{u.roles.length === 0 ? (
											<em className="text-neutral-400">none</em>
										) : (
											<div className="flex flex-wrap gap-1">
												{u.roles.map((r) => (
													<span
														key={r.id}
														className="rounded bg-neutral-200 px-2 py-0.5 text-[10px] uppercase tracking-wide"
													>
														{r.name}
													</span>
												))}
											</div>
										)}
									</td>
									<td className="px-3 py-2 text-xs text-neutral-500">
										{new Date(u.created_at).toLocaleDateString()}
									</td>
									<td className="px-3 py-2">
										<Link
											to="/admin/users/$id"
											params={{ id: u.id }}
											search={{ role: "" }}
											className="text-xs text-blue-700 hover:underline"
										>
											Edit
										</Link>
									</td>
								</tr>
							))}
							{users.length === 0 && (
								<tr>
									<td colSpan={7} className="px-3 py-6 text-center text-sm text-neutral-500">
										No users match the filter.
									</td>
								</tr>
							)}
						</tbody>
					</table>
				</div>
			)}

			<ConfirmDialog
				open={confirm !== null}
				title={
					confirm?.kind === "assign"
						? "Assign role?"
						: confirm?.kind === "remove"
							? "Remove role?"
							: ""
				}
				message={
					confirm
						? confirm.kind === "assign"
							? `Assign the "${confirm.roleName}" role to ${confirm.users.length} user(s)?`
							: `Remove the "${confirm.roleName}" role from ${confirm.users.length} user(s)? This cannot be undone without reassigning manually.`
						: ""
				}
				busy={assignMutation.isPending || revokeMutation.isPending}
				onConfirm={() => {
					if (!confirm) return;
					if (confirm.kind === "assign") {
						assignMutation.mutate({
							userIds: confirm.users.map((u) => u.id),
							roleId: confirm.roleId,
						});
					} else {
						revokeMutation.mutate({
							userIds: confirm.users.map((u) => u.id),
							roleId: confirm.roleId,
						});
					}
					setConfirm(null);
				}}
				onCancel={() => setConfirm(null)}
			/>
		</main>
	);
}

function Header() {
	return (
		<header className="mb-6 border-b border-neutral-200 pb-4">
			<p className="font-mono text-xs uppercase tracking-wide text-neutral-500">Admin</p>
			<h1 className="mt-1 text-3xl font-semibold tracking-tight">Users</h1>
			<p className="mt-2 text-sm text-neutral-600">
				Search, filter, and reassign roles. Use the checkboxes for bulk role changes.
			</p>
		</header>
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
			{[0, 1, 2, 3].map((i) => (
				<div key={i} className="h-10 animate-pulse rounded bg-neutral-100" />
			))}
		</div>
	);
}

function ErrorMain({ message }: { message: string }) {
	return (
		<main className="mx-auto max-w-5xl px-6 py-10">
			<h1 className="mb-4 text-2xl font-semibold">Users</h1>
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
