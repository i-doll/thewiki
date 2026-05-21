import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { Editor } from "../components/Editor";

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

const DEMO_INITIAL = "# Hello\n\nWorld";

function HomeComponent() {
	const health = useQuery({
		queryKey: ["healthz"],
		queryFn: fetchHealth,
	});

	const [markdown, setMarkdown] = useState<string>(DEMO_INITIAL);

	return (
		<main className="mx-auto flex max-w-3xl flex-col gap-6 px-6 py-16">
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

			<section className="flex flex-col gap-3">
				<div>
					<h2 className="text-sm font-medium uppercase tracking-wide text-neutral-500">
						Hybrid editor demo
					</h2>
					<p className="mt-1 text-xs text-neutral-500">
						Demo only — the real edit page lands in #18. Toggle between WYSIWYG and source mode; the
						Markdown round-trips between both.
					</p>
				</div>
				<Editor value={markdown} onChange={setMarkdown} />
				<details className="rounded-md border border-neutral-200 bg-white">
					<summary className="cursor-pointer px-3 py-2 text-xs font-medium text-neutral-600">
						Live Markdown source
					</summary>
					<pre className="overflow-x-auto border-t border-neutral-200 bg-neutral-50 px-3 py-2 font-mono text-xs text-neutral-800">
						{markdown}
					</pre>
				</details>
			</section>
		</main>
	);
}
