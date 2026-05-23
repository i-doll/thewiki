//! Read-only runtime config viewer (`/admin/config`) — #47.
//!
//! Renders the redacted JSON the server returns. No mutation: the
//! authoritative source is the TOML file the operator deployed; this
//! page is purely a "what did I actually load?" surface.

import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import {
	type AdminConfigResponse,
	type ApiError,
	type AuthMePayload,
	fetchAdminConfig,
	fetchAuthMe,
	parsePermissions,
} from "../lib/api";

export const Route = createFileRoute("/admin/config")({
	component: ConfigComponent,
});

function ConfigComponent() {
	const meQuery = useQuery<AuthMePayload | null>({
		queryKey: ["auth-me"],
		queryFn: fetchAuthMe,
	});
	if (meQuery.isPending) return <Skeleton />;
	if (meQuery.isError) return <ErrorMain message={meQuery.error.message} />;
	const me = meQuery.data;
	if (!me) return <ErrorMain message="You must sign in to view configuration." />;
	const perms = parsePermissions(me.permissions);
	if (!perms.has("MANAGE_USERS"))
		return (
			<ErrorMain message="You do not have the MANAGE_USERS permission required to view configuration." />
		);
	return <ConfigPanel />;
}

function ConfigPanel() {
	const query = useQuery<AdminConfigResponse, ApiError>({
		queryKey: ["admin-config"],
		queryFn: fetchAdminConfig,
	});

	return (
		<main className="mx-auto max-w-5xl px-6 py-10">
			<header className="mb-6 border-b border-neutral-200 pb-4">
				<p className="font-mono text-xs uppercase tracking-wide text-neutral-500">Admin</p>
				<h1 className="mt-1 text-3xl font-semibold tracking-tight">Configuration</h1>
				<p className="mt-2 text-sm text-neutral-600">
					Live snapshot of the runtime configuration this binary booted with. Secrets are redacted.
					To change a value, edit the TOML file and restart.
				</p>
			</header>

			{query.isPending && <Skeleton />}
			{query.isError && <ErrorBox>Failed to load config: {query.error.message}</ErrorBox>}
			{query.data && !query.data.available && (
				<ErrorBox>
					This deployment did not wire a runtime configuration snapshot. The viewer is only
					available when the binary is booted via the <code>serve</code> subcommand.
				</ErrorBox>
			)}
			{query.data?.available && (
				<pre className="overflow-x-auto rounded-md border border-neutral-200 bg-neutral-950 p-4 font-mono text-xs leading-relaxed text-neutral-100">
					{JSON.stringify(query.data.config, null, 2)}
				</pre>
			)}
		</main>
	);
}

function Skeleton() {
	return <div className="h-72 animate-pulse rounded bg-neutral-100" />;
}

function ErrorMain({ message }: { message: string }) {
	return (
		<main className="mx-auto max-w-5xl px-6 py-10">
			<h1 className="mb-4 text-2xl font-semibold">Configuration</h1>
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
