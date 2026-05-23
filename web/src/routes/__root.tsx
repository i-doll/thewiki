import { type QueryClient, useQuery } from "@tanstack/react-query";
import { createRootRouteWithContext, Link, Outlet } from "@tanstack/react-router";
import { Toaster } from "react-hot-toast";
import { InboxBell } from "../components/InboxBell";
import { SearchBox } from "../components/SearchBox";
import {
	type AuthMePayload,
	fetchAuthMe,
	hasAnyAdminPermission,
	parsePermissions,
} from "../lib/api";

interface RouterContext {
	queryClient: QueryClient;
}

export const Route = createRootRouteWithContext<RouterContext>()({
	component: RootComponent,
});

function RootComponent() {
	// The Admin link only appears for users with at least one admin
	// permission. Resolving via `fetchAuthMe` keeps the gate consistent
	// with what the admin pages themselves use, so a 401 / lost session
	// hides the link automatically.
	const meQuery = useQuery<AuthMePayload | null>({
		queryKey: ["auth-me"],
		queryFn: fetchAuthMe,
	});
	const me = meQuery.data ?? null;
	const showAdmin = me !== null && hasAnyAdminPermission(parsePermissions(me.permissions));

	return (
		<div className="min-h-full bg-neutral-50 text-neutral-900">
			<header className="border-b border-neutral-200 bg-white">
				<div className="mx-auto flex max-w-5xl items-center justify-between gap-4 px-6 py-3">
					<Link to="/" className="text-sm font-semibold tracking-tight text-neutral-900">
						thewiki
					</Link>
					<SearchBox />
					<nav className="flex items-center gap-4 text-sm text-neutral-600">
						<Link to="/wiki" className="hover:text-neutral-900">
							Pages
						</Link>
						<Link to="/watchlist" className="hover:text-neutral-900">
							Watchlist
						</Link>
						{showAdmin && (
							<Link to="/admin" className="hover:text-neutral-900">
								Admin
							</Link>
						)}
						<InboxBell />
						<Link to="/login" className="hover:text-neutral-900">
							Login
						</Link>
						<Link to="/register" className="hover:text-neutral-900">
							Register
						</Link>
					</nav>
				</div>
			</header>
			<Outlet />
			<Toaster position="bottom-right" />
		</div>
	);
}
