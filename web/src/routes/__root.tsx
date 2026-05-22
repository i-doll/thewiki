import type { QueryClient } from "@tanstack/react-query";
import { createRootRouteWithContext, Link, Outlet } from "@tanstack/react-router";
import { Toaster } from "react-hot-toast";

interface RouterContext {
	queryClient: QueryClient;
}

export const Route = createRootRouteWithContext<RouterContext>()({
	component: RootComponent,
});

function RootComponent() {
	return (
		<div className="min-h-full bg-neutral-50 text-neutral-900">
			<header className="border-b border-neutral-200 bg-white">
				<div className="mx-auto flex max-w-5xl items-center justify-between px-6 py-3">
					<Link to="/" className="text-sm font-semibold tracking-tight text-neutral-900">
						thewiki
					</Link>
					<nav className="flex items-center gap-4 text-sm text-neutral-600">
						<Link to="/wiki" className="hover:text-neutral-900">
							Pages
						</Link>
						<Link to="/login" className="hover:text-neutral-900">
							Login
						</Link>
					</nav>
				</div>
			</header>
			<Outlet />
			<Toaster position="bottom-right" />
		</div>
	);
}
