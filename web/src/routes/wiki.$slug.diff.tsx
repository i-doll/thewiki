import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { useState } from "react";
import ReactDiffViewer, { DiffMethod } from "react-diff-viewer-continued";

/**
 * Diff view comparing two revisions of a page. Accepts `?from=<rev>&to=<rev>`
 * query parameters so URLs are deep-linkable.
 *
 * Two display modes:
 *   * Unified — render the server-supplied `unified` string verbatim in a
 *     monospace `<pre>`. Cheap and gives the user something they can copy
 *     into a bug report.
 *   * Split — side-by-side diff via `react-diff-viewer-continued`. We feed
 *     it the full bodies reconstructed from the hunks so it can compute its
 *     own line-up rather than us hand-rolling a split renderer.
 */
export const Route = createFileRoute("/wiki/$slug/diff")({
	component: DiffComponent,
	validateSearch: (search: Record<string, unknown>) => {
		// Both `from` and `to` are required; pass through whatever the URL
		// provided (validation happens lazily inside the component so we can
		// render a useful error instead of a router exception).
		return {
			from: typeof search.from === "string" ? search.from : "",
			to: typeof search.to === "string" ? search.to : "",
		};
	},
});

type DiffKind = "context" | "insertion" | "deletion";

interface DiffLine {
	kind: DiffKind;
	content: string;
}

interface DiffHunk {
	old_start: number;
	old_count: number;
	new_start: number;
	new_count: number;
	lines: DiffLine[];
}

interface DiffResponse {
	from: string;
	to: string;
	unified: string;
	hunks: DiffHunk[];
}

interface ApiErrorBody {
	message?: string;
}

async function fetchDiff(slug: string, from: string, to: string): Promise<DiffResponse> {
	const params = new URLSearchParams({ from, to });
	const res = await fetch(`/api/v1/pages/${encodeURIComponent(slug)}/diff?${params.toString()}`);
	if (!res.ok) {
		let detail = res.statusText;
		try {
			const body = (await res.json()) as ApiErrorBody;
			if (body.message) {
				detail = body.message;
			}
		} catch {
			// Non-JSON body; fall through.
		}
		throw new Error(`Failed to load diff: ${detail}`);
	}
	return (await res.json()) as DiffResponse;
}

/**
 * Reconstruct the `old` and `new` text from the structured hunks.
 *
 * `react-diff-viewer-continued` wants the full before/after strings and runs
 * its own diff. We don't have those on the wire — only the hunks — so we
 * stitch a best-effort reconstruction by concatenating context+deletion lines
 * for the `old` side and context+insertion lines for the `new` side. This is
 * lossy across hunks (skipped context between hunks is missing) but for short
 * pages the result is fine; for longer pages users can still fall back to the
 * unified view.
 */
function reconstructSides(hunks: DiffHunk[]): { oldText: string; newText: string } {
	const oldParts: string[] = [];
	const newParts: string[] = [];
	hunks.forEach((hunk, idx) => {
		if (idx > 0) {
			// Visually separate non-contiguous hunks so users can see we elided
			// unchanged content between them.
			oldParts.push("…\n");
			newParts.push("…\n");
		}
		for (const line of hunk.lines) {
			if (line.kind === "context") {
				oldParts.push(line.content);
				newParts.push(line.content);
			} else if (line.kind === "deletion") {
				oldParts.push(line.content);
			} else if (line.kind === "insertion") {
				newParts.push(line.content);
			}
		}
	});
	return { oldText: oldParts.join(""), newText: newParts.join("") };
}

function shortenId(id: string): string {
	return id.length <= 8 ? id : id.slice(0, 8);
}

type ViewMode = "unified" | "split";

function DiffComponent() {
	const { slug } = Route.useParams();
	const search = Route.useSearch();
	const [mode, setMode] = useState<ViewMode>("split");

	const hasValidParams = search.from.length > 0 && search.to.length > 0;

	const query = useQuery({
		queryKey: ["diff", slug, search.from, search.to],
		queryFn: () => fetchDiff(slug, search.from, search.to),
		enabled: hasValidParams,
	});

	return (
		<main className="mx-auto flex max-w-6xl flex-col gap-6 px-6 py-10">
			<header className="flex flex-col gap-2">
				<nav className="text-xs text-neutral-500">
					<Link to="/" className="hover:text-neutral-700">
						home
					</Link>
					{" / "}
					<span>wiki</span>
					{" / "}
					<span className="font-mono">{slug}</span>
					{" / "}
					<Link to="/wiki/$slug/history" params={{ slug }} className="hover:text-neutral-700">
						history
					</Link>
					{" / "}
					<span>diff</span>
				</nav>
				<h1 className="text-2xl font-semibold tracking-tight">
					Diff — <span className="font-mono">{slug}</span>
				</h1>
				{hasValidParams && (
					<p className="font-mono text-xs text-neutral-500">
						{shortenId(search.from)} → {shortenId(search.to)}
					</p>
				)}
			</header>

			<div className="flex items-center gap-2">
				<fieldset className="inline-flex overflow-hidden rounded-md border border-neutral-300 text-xs">
					<legend className="sr-only">Diff display mode</legend>
					<button
						type="button"
						onClick={() => setMode("split")}
						aria-pressed={mode === "split"}
						className={
							mode === "split"
								? "bg-neutral-900 px-3 py-1 font-medium text-white"
								: "bg-white px-3 py-1 text-neutral-700 hover:bg-neutral-100"
						}
					>
						Split
					</button>
					<button
						type="button"
						onClick={() => setMode("unified")}
						aria-pressed={mode === "unified"}
						className={
							mode === "unified"
								? "bg-neutral-900 px-3 py-1 font-medium text-white"
								: "bg-white px-3 py-1 text-neutral-700 hover:bg-neutral-100"
						}
					>
						Unified
					</button>
				</fieldset>
				<Link
					to="/wiki/$slug/history"
					params={{ slug }}
					className="text-xs text-neutral-600 hover:text-neutral-900"
				>
					← back to history
				</Link>
			</div>

			{!hasValidParams && (
				<div className="rounded-md border border-amber-200 bg-amber-50 p-4 text-sm text-amber-800">
					Missing <code className="font-mono">from</code> or <code className="font-mono">to</code>{" "}
					query parameter. Pick two revisions from the history page.
				</div>
			)}

			{hasValidParams && query.isPending && (
				<div className="rounded-md border border-neutral-200 bg-white p-4 text-sm text-neutral-500">
					Loading diff…
				</div>
			)}

			{hasValidParams && query.isError && (
				<div className="rounded-md border border-red-200 bg-red-50 p-4 text-sm text-red-700">
					{query.error instanceof Error ? query.error.message : "Failed to load diff"}
				</div>
			)}

			{query.isSuccess &&
				(query.data.hunks.length === 0 ? (
					<div className="rounded-md border border-neutral-200 bg-white p-4 text-sm text-neutral-500">
						No differences between the two revisions.
					</div>
				) : mode === "unified" ? (
					<UnifiedView unified={query.data.unified} />
				) : (
					<SplitView hunks={query.data.hunks} from={query.data.from} to={query.data.to} />
				))}
		</main>
	);
}

function UnifiedView({ unified }: { unified: string }) {
	// Tokenise per line so we can colour additions/deletions while still
	// rendering the whole blob as a single scrollable block. Trailing newlines
	// in `unified` produce an empty final entry — drop it so we don't render an
	// empty row.
	const lines = unified.split("\n");
	if (lines.length > 0 && lines[lines.length - 1] === "") {
		lines.pop();
	}
	return (
		<pre className="overflow-x-auto rounded-md border border-neutral-200 bg-neutral-50 p-4 font-mono text-xs leading-relaxed text-neutral-800">
			{lines.map((line, idx) => {
				const kind = classifyLine(line);
				const className =
					kind === "insertion"
						? "block bg-green-50 text-green-900"
						: kind === "deletion"
							? "block bg-red-50 text-red-900"
							: kind === "header"
								? "block text-neutral-500"
								: "block";
				// Compose a key from `idx` + a slice of the line content so the
				// react reconciler can distinguish repeated identical lines
				// (very common in diff context blocks). The list is fully replaced
				// on every diff fetch, so cross-render key stability only matters
				// within a single payload.
				const key = `${idx}:${line.slice(0, 32)}`;
				return (
					<span key={key} className={className}>
						{line || " "}
					</span>
				);
			})}
		</pre>
	);
}

type UnifiedLineKind = "header" | "insertion" | "deletion" | "context";

function classifyLine(line: string): UnifiedLineKind {
	// `+++` / `---` are the file-header lines emitted at the top of every
	// unified diff. Check them before the single-char prefixes so they don't
	// get coloured as additions/deletions.
	if (line.startsWith("+++") || line.startsWith("---") || line.startsWith("@@")) {
		return "header";
	}
	if (line.startsWith("+")) {
		return "insertion";
	}
	if (line.startsWith("-")) {
		return "deletion";
	}
	return "context";
}

function SplitView({ hunks, from, to }: { hunks: DiffHunk[]; from: string; to: string }) {
	const { oldText, newText } = reconstructSides(hunks);
	return (
		<div className="overflow-hidden rounded-md border border-neutral-200">
			<ReactDiffViewer
				oldValue={oldText}
				newValue={newText}
				splitView={true}
				compareMethod={DiffMethod.LINES}
				leftTitle={`from ${shortenId(from)}`}
				rightTitle={`to ${shortenId(to)}`}
				useDarkTheme={false}
			/>
		</div>
	);
}
