//! Admin landing page (`/admin`) — #47.
//!
//! Renders a tile grid linking to every admin sub-page the calling session
//! has permission for. The page itself is gated by *any* admin permission;
//! tiles are filtered to the bits the user actually holds.

import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import {
	type AuthMePayload,
	fetchAuthMe,
	hasAnyAdminPermission,
	parsePermissions,
} from "../lib/api";

export const Route = createFileRoute("/admin/")({
	component: AdminLanding,
});

interface Tile {
	title: string;
	description: string;
	to: string;
	requires: string;
	search?: Record<string, unknown>;
}

const TILES: Tile[] = [
	{
		title: "Users",
		description: "Search users, assign / remove roles, bulk operations.",
		to: "/admin/users",
		requires: "MANAGE_USERS",
	},
	{
		title: "Roles",
		description: "Create and edit roles, manage their permission flags.",
		to: "/admin/roles",
		requires: "MANAGE_ROLES",
	},
	{
		title: "Namespaces",
		description: "Create namespaces, rename their display labels, remove empties.",
		to: "/admin/namespaces",
		requires: "MANAGE_NAMESPACES",
	},
	{
		title: "Page protection",
		description: "Search pages and adjust their protection level (single + bulk).",
		to: "/admin/protection",
		requires: "PROTECT",
	},
	{
		title: "Approval queue",
		description: "Review pending edits before they go live on the wiki.",
		to: "/admin/approval-queue",
		requires: "REVIEW_EDITS",
	},
	{
		title: "Blocklists",
		description: "Manage the IP CIDR and URL pattern blocklists.",
		to: "/admin/blocklists",
		requires: "MANAGE_BLOCKLIST",
	},
	{
		title: "Audit log",
		description: "Inspect the administrative audit trail (with Atom feed).",
		to: "/admin/audit-log",
		requires: "VIEW_AUDIT_LOG",
	},
	{
		title: "Configuration",
		description: "Read-only viewer of the loaded runtime configuration.",
		to: "/admin/config",
		// MANAGE_USERS chosen as the closest existing "real admin"
		// permission per #47 (which explicitly asks us not to add a new
		// flag for the viewer). Widen later if a dedicated
		// MANAGE_CONFIG bit lands — `MANAGE_ROLES`-only admins can't see
		// this tile today.
		requires: "MANAGE_USERS",
	},
];

function AdminLanding() {
	const meQuery = useQuery<AuthMePayload | null>({
		queryKey: ["auth-me"],
		queryFn: fetchAuthMe,
	});

	if (meQuery.isPending) {
		return <Skeleton />;
	}
	if (meQuery.isError) {
		return (
			<main className="mx-auto max-w-5xl px-6 py-10">
				<ErrorBox>Failed to load session: {meQuery.error.message}</ErrorBox>
			</main>
		);
	}
	const me = meQuery.data;
	if (!me) {
		return (
			<main className="mx-auto max-w-5xl px-6 py-10">
				<h1 className="mb-4 text-2xl font-semibold">Admin</h1>
				<ErrorBox>You must sign in to access the admin section.</ErrorBox>
			</main>
		);
	}
	const perms = parsePermissions(me.permissions);
	if (!hasAnyAdminPermission(perms)) {
		return (
			<main className="mx-auto max-w-5xl px-6 py-10">
				<h1 className="mb-4 text-2xl font-semibold">Admin</h1>
				<ErrorBox>You do not have permission to access the admin section.</ErrorBox>
			</main>
		);
	}

	const visible = TILES.filter((tile) => perms.has(tile.requires));
	return (
		<main className="mx-auto max-w-5xl px-6 py-10">
			<header className="mb-8 border-b border-neutral-200 pb-4">
				<p className="font-mono text-xs uppercase tracking-wide text-neutral-500">Admin</p>
				<h1 className="mt-1 text-3xl font-semibold tracking-tight">Wiki administration</h1>
				<p className="mt-2 text-sm text-neutral-600">
					Tooling for operators with elevated permissions. Tiles only appear when your session
					carries the matching capability.
				</p>
			</header>

			<div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
				{visible.map((tile) => (
					<TileCard key={tile.to} tile={tile} />
				))}
			</div>
		</main>
	);
}

function TileCard({ tile }: { tile: Tile }) {
	// The approval-queue route requires a `search` initial value; everything
	// else takes no search params. TanStack Router's typed `Link` doesn't
	// gracefully accept a polymorphic `search` so we branch.
	if (tile.to === "/admin/approval-queue") {
		return (
			<Link
				to="/admin/approval-queue"
				search={{ status: "pending", selected: "" }}
				className="block rounded-md border border-neutral-200 bg-white p-4 transition hover:border-neutral-400 hover:shadow"
			>
				<h2 className="text-base font-semibold text-neutral-900">{tile.title}</h2>
				<p className="mt-1 text-sm text-neutral-600">{tile.description}</p>
				<p className="mt-2 font-mono text-[10px] uppercase tracking-wide text-neutral-400">
					Requires {tile.requires}
				</p>
			</Link>
		);
	}
	return (
		<Link
			to={tile.to}
			className="block rounded-md border border-neutral-200 bg-white p-4 transition hover:border-neutral-400 hover:shadow"
		>
			<h2 className="text-base font-semibold text-neutral-900">{tile.title}</h2>
			<p className="mt-1 text-sm text-neutral-600">{tile.description}</p>
			<p className="mt-2 font-mono text-[10px] uppercase tracking-wide text-neutral-400">
				Requires {tile.requires}
			</p>
		</Link>
	);
}

function Skeleton() {
	return (
		<main className="mx-auto max-w-5xl px-6 py-10">
			<div className="h-8 w-1/3 animate-pulse rounded bg-neutral-200" />
			<div className="mt-6 grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
				{[0, 1, 2, 3, 4, 5].map((i) => (
					<div key={i} className="h-24 animate-pulse rounded bg-neutral-100" />
				))}
			</div>
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
