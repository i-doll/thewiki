import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/")({
	component: HomeComponent,
});

interface HealthResponse {
	status: string;
}

async function fetchHealth(): Promise<HealthResponse> {
	const res = await fetch("/api/v1/healthz");
	if (!res.ok) {
		throw new Error(`Health check failed: ${res.status} ${res.statusText}`);
	}
	return (await res.json()) as HealthResponse;
}

function HomeComponent() {
	const health = useQuery({
		queryKey: ["healthz"],
		queryFn: fetchHealth,
	});

	return (
		<main className="mx-auto flex max-w-2xl flex-col gap-6 px-6 py-16">
			<header>
				<h1 className="text-3xl font-semibold tracking-tight">thewiki — pre-alpha</h1>
				<p className="mt-2 text-neutral-600">
					Frontend scaffold. TanStack Router + Query + Vite + Tailwind.
				</p>
			</header>

			<section className="rounded-lg border border-neutral-200 bg-white p-4">
				<h2 className="text-sm font-medium uppercase tracking-wide text-neutral-500">
					Backend health
				</h2>
				<div className="mt-2 font-mono text-sm">
					{health.isPending && <span className="text-neutral-500">Loading…</span>}
					{health.isError && <span className="text-red-600">Error: {health.error.message}</span>}
					{health.isSuccess && (
						<span className="text-green-700">status = {health.data.status}</span>
					)}
				</div>
			</section>
		</main>
	);
}
