//! Per-user admin detail (`/admin/users/{id}`) — #47.
//!
//! Shows the user's metadata and a multi-select role editor. Saving
//! diffs against the current assignments and issues the necessary
//! assign / revoke calls, each gated by a confirmation modal when the
//! revoke list is non-empty.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { useEffect, useMemo, useState } from "react";
import { ConfirmDialog } from "../components/ConfirmDialog";
import {
	type AdminRoleListResponse,
	type AdminUserView,
	type ApiError,
	type AuthMePayload,
	assignUserRoles,
	fetchAdminUser,
	fetchAuthMe,
	listAdminRoles,
	parsePermissions,
	revokeUserRole,
} from "../lib/api";

export const Route = createFileRoute("/admin/users/$id")({
	component: UserDetailComponent,
});

function UserDetailComponent() {
	const { id } = Route.useParams();
	const meQuery = useQuery<AuthMePayload | null>({
		queryKey: ["auth-me"],
		queryFn: fetchAuthMe,
	});
	if (meQuery.isPending) return <Skeleton />;
	if (meQuery.isError) return <ErrorMain message={meQuery.error.message} />;
	const me = meQuery.data;
	if (!me) return <ErrorMain message="You must sign in to manage users." />;
	const perms = parsePermissions(me.permissions);
	if (!perms.has("MANAGE_USERS"))
		return <ErrorMain message="You do not have the MANAGE_USERS permission." />;

	return <Editor userId={id} />;
}

function Editor({ userId }: { userId: string }) {
	const qc = useQueryClient();
	const userQuery = useQuery<AdminUserView, ApiError>({
		queryKey: ["admin-user", userId],
		queryFn: () => fetchAdminUser(userId),
	});
	const rolesQuery = useQuery<AdminRoleListResponse, ApiError>({
		queryKey: ["admin-roles"],
		queryFn: listAdminRoles,
	});

	const [selected, setSelected] = useState<Set<string>>(new Set());
	const [pendingRevokeIds, setPendingRevokeIds] = useState<string[] | null>(null);

	useEffect(() => {
		if (userQuery.data) {
			setSelected(new Set(userQuery.data.roles.map((r) => r.id)));
		}
	}, [userQuery.data]);

	const currentIds = useMemo(
		() => new Set(userQuery.data?.roles.map((r) => r.id) ?? []),
		[userQuery.data],
	);
	const toAdd = useMemo(
		() => [...selected].filter((id) => !currentIds.has(id)),
		[selected, currentIds],
	);
	const toRevoke = useMemo(
		() => [...currentIds].filter((id) => !selected.has(id)),
		[selected, currentIds],
	);

	const saveMutation = useMutation({
		mutationFn: async () => {
			if (toAdd.length > 0) {
				await assignUserRoles(userId, { role_ids: toAdd });
			}
			for (const rid of toRevoke) {
				await revokeUserRole(userId, rid);
			}
		},
		onSuccess: () => {
			setPendingRevokeIds(null);
			void qc.invalidateQueries({ queryKey: ["admin-user", userId] });
			void qc.invalidateQueries({ queryKey: ["admin-users"] });
		},
	});

	const handleSave = () => {
		if (toAdd.length === 0 && toRevoke.length === 0) return;
		if (toRevoke.length > 0) {
			// Removal step always confirms.
			setPendingRevokeIds(toRevoke);
			return;
		}
		saveMutation.mutate();
	};

	if (userQuery.isPending || rolesQuery.isPending) return <Skeleton />;
	if (userQuery.isError) return <ErrorMain message={userQuery.error.message} />;
	if (rolesQuery.isError) return <ErrorMain message={rolesQuery.error.message} />;

	const user = userQuery.data;
	const roles = rolesQuery.data?.items ?? [];

	return (
		<main className="mx-auto max-w-3xl px-6 py-10">
			<nav className="mb-4 text-xs text-neutral-500">
				<Link to="/admin/users" search={{ role: "" }} className="hover:underline">
					← Back to users
				</Link>
			</nav>
			<header className="mb-6 border-b border-neutral-200 pb-4">
				<p className="font-mono text-xs uppercase tracking-wide text-neutral-500">Admin</p>
				<h1 className="mt-1 text-3xl font-semibold tracking-tight">{user.username}</h1>
				<p className="mt-2 text-sm text-neutral-600">
					{user.display_name ?? <em>no display name</em>} · {user.email ?? <em>no email</em>}
				</p>
			</header>

			<section className="mb-6 rounded-md border border-neutral-200 bg-white p-4">
				<h2 className="mb-3 text-sm font-semibold uppercase tracking-wide text-neutral-500">
					Roles
				</h2>
				<div className="space-y-2">
					{roles.map((role) => {
						const checked = selected.has(role.id);
						return (
							<label
								key={role.id}
								className="flex items-center gap-3 rounded border border-neutral-200 px-3 py-2 hover:bg-neutral-50"
							>
								<input
									type="checkbox"
									checked={checked}
									onChange={() => {
										const next = new Set(selected);
										if (checked) next.delete(role.id);
										else next.add(role.id);
										setSelected(next);
									}}
								/>
								<div className="flex-1">
									<div className="text-sm font-medium">{role.display_name}</div>
									<div className="mt-1 flex flex-wrap gap-1">
										{role.permission_flags.map((flag) => (
											<span
												key={flag}
												className="rounded bg-neutral-200 px-2 py-0.5 text-[10px] uppercase tracking-wide"
											>
												{flag}
											</span>
										))}
										{role.permission_flags.length === 0 && (
											<em className="text-[10px] text-neutral-400">no flags</em>
										)}
									</div>
								</div>
							</label>
						);
					})}
				</div>
				<div className="mt-4 flex justify-end gap-2">
					<button
						type="button"
						onClick={() => setSelected(currentIds)}
						className="rounded border border-neutral-300 bg-white px-4 py-2 text-sm hover:bg-neutral-50"
					>
						Reset
					</button>
					<button
						type="button"
						onClick={handleSave}
						disabled={saveMutation.isPending || (toAdd.length === 0 && toRevoke.length === 0)}
						className="rounded bg-neutral-900 px-4 py-2 text-sm font-medium text-white disabled:opacity-50"
					>
						Save changes
					</button>
				</div>
				{saveMutation.isError && (
					<p className="mt-2 text-sm text-red-700">{(saveMutation.error as ApiError).message}</p>
				)}
			</section>

			<ConfirmDialog
				open={pendingRevokeIds !== null}
				title="Remove role assignments?"
				message={(() => {
					const ids = pendingRevokeIds ?? [];
					const names = roles.filter((r) => ids.includes(r.id)).map((r) => r.display_name);
					const removed = names.join(", ");
					if (toAdd.length > 0) {
						const added = roles
							.filter((r) => toAdd.includes(r.id))
							.map((r) => r.display_name)
							.join(", ");
						return `Remove ${removed} and add ${added}? This cannot be undone.`;
					}
					return `Remove ${removed} from ${user.username}? This cannot be undone.`;
				})()}
				busy={saveMutation.isPending}
				onConfirm={() => saveMutation.mutate()}
				onCancel={() => setPendingRevokeIds(null)}
			/>
		</main>
	);
}

function Skeleton() {
	return (
		<main className="mx-auto max-w-3xl px-6 py-10">
			<div className="h-8 w-1/3 animate-pulse rounded bg-neutral-200" />
			<div className="mt-6 space-y-2">
				{[0, 1, 2, 3].map((i) => (
					<div key={i} className="h-10 animate-pulse rounded bg-neutral-100" />
				))}
			</div>
		</main>
	);
}

function ErrorMain({ message }: { message: string }) {
	return (
		<main className="mx-auto max-w-3xl px-6 py-10">
			<h1 className="mb-4 text-2xl font-semibold">User</h1>
			<div className="rounded-md border border-red-300 bg-red-50 p-4 text-sm text-red-700">
				{message}
			</div>
		</main>
	);
}
