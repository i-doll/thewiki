//! Threaded view for talk-namespace pages (#43).
//!
//! Server-side we store the talk page as plain markdown — the API does not
//! parse threads (matching the design constraint). Here on the SPA we
//! split the body into top-level threads on `## ` headings and treat
//! `> `-prefixed paragraphs inside each thread as nested replies. The
//! result renders as an indented discussion tree without rewriting the
//! underlying markdown.
//!
//! Markdown rendering still goes through the existing `renderMarkdown`
//! helper so wikilinks, signatures, and sanitisation behave identically
//! to the subject-page view.

import { useMemo } from "react";
import { renderMarkdown } from "../lib/markdown";

/**
 * Parsed thread tree returned by [`parseThreads`]. Exported only for the
 * test module that exercises the parser directly.
 */
export interface Thread {
	/** Heading text (everything on the `## …` line after the marker). */
	title: string;
	/**
	 * Lines belonging to the thread before any reply blocks. Joined back
	 * into a markdown blob for rendering.
	 */
	body: string;
	/** Nested replies, parsed from blockquote-with-author paragraphs. */
	replies: Reply[];
}

/** One reply inside a thread — a paragraph that started with `> `. */
export interface Reply {
	/**
	 * Indentation depth — `1` for `> reply`, `2` for `>> reply to reply`,
	 * and so on. Capped at 8 levels so the rendered tree never overflows
	 * the viewport.
	 */
	depth: number;
	/** Reply body with the leading `>` markers stripped, ready for render. */
	body: string;
}

const MAX_DEPTH = 8;

/**
 * Split `body` into top-level threads on `## ` headings.
 *
 * Anything before the first `##` is treated as the page's preamble and
 * surfaced as a synthetic thread whose title is the empty string; the
 * renderer hides it from the thread list when no preamble exists.
 */
export function parseThreads(body: string): Thread[] {
	const lines = body.split(/\r?\n/);
	const threads: Thread[] = [];
	let current: Thread | null = null;
	let bodyBuf: string[] = [];

	const flush = () => {
		if (current === null) {
			return;
		}
		const joined = bodyBuf.join("\n");
		const { preamble, replies } = extractReplies(joined);
		current.body = preamble.trim();
		current.replies = replies;
		threads.push(current);
		bodyBuf = [];
		current = null;
	};

	for (const line of lines) {
		const heading = /^##\s+(.+?)\s*$/.exec(line);
		if (heading !== null) {
			flush();
			current = { title: heading[1] ?? "", body: "", replies: [] };
			continue;
		}
		if (current === null) {
			current = { title: "", body: "", replies: [] };
		}
		bodyBuf.push(line);
	}
	flush();
	return threads;
}

/**
 * Walk a thread body and pull every `> `-prefixed paragraph out as a
 * [`Reply`] with depth equal to the count of leading `>` markers.
 */
function extractReplies(body: string): { preamble: string; replies: Reply[] } {
	const paragraphs = body.split(/\n\n+/);
	const preamble: string[] = [];
	const replies: Reply[] = [];
	for (const para of paragraphs) {
		const stripped = para.trimStart();
		if (stripped.startsWith(">")) {
			replies.push(parseReply(para));
		} else if (replies.length === 0) {
			preamble.push(para);
		} else {
			// A non-blockquote paragraph after the first reply is treated
			// as a "back to the root" continuation — we attach it to the
			// last reply at depth 0 so authors can interleave reactions
			// without losing the thread context.
			replies.push({ depth: 1, body: para.trim() });
		}
	}
	return { preamble: preamble.join("\n\n"), replies };
}

function parseReply(paragraph: string): Reply {
	const lines = paragraph.split(/\r?\n/);
	let depth = 0;
	const cleaned: string[] = [];
	for (const line of lines) {
		const m = /^(>+)\s?(.*)$/.exec(line);
		if (m !== null) {
			const markers = m[1] ?? "";
			depth = Math.max(depth, markers.length);
			cleaned.push(m[2] ?? "");
		} else {
			cleaned.push(line);
		}
	}
	return {
		depth: Math.min(depth, MAX_DEPTH),
		body: cleaned.join("\n").trim(),
	};
}

interface TalkThreadProps {
	body: string;
}

/**
 * Render a talk page as a list of threads + nested replies. Each thread
 * heading becomes a `<section>` and each reply renders as a markdown
 * block indented by `depth × 1.25rem`.
 */
export function TalkThread({ body }: TalkThreadProps) {
	const threads = useMemo(() => parseThreads(body), [body]);

	if (threads.length === 0) {
		return (
			<p className="text-sm italic text-neutral-500">
				No discussion yet. Start a new thread with a <code>## Heading</code>.
			</p>
		);
	}

	return (
		<div className="flex flex-col gap-6">
			{threads.map((thread, idx) => (
				<section
					// We intentionally don't have a stable identifier per thread —
					// the title is user-supplied and may repeat, so we fall back
					// to the array index. React's reconciliation still works
					// correctly because thread order is stable within one render.
					// eslint-disable-next-line react/no-array-index-key
					key={`thread-${idx}-${thread.title}`}
					className="rounded-md border border-neutral-200 bg-white"
				>
					{thread.title.length > 0 && (
						<header className="border-b border-neutral-200 px-4 py-2">
							<h2 className="text-lg font-semibold tracking-tight text-neutral-900">
								{thread.title}
							</h2>
						</header>
					)}
					{thread.body.length > 0 && (
						<div
							className="prose max-w-none px-4 py-3 text-sm"
							// biome-ignore lint/security/noDangerouslySetInnerHtml: rendered + sanitised through renderMarkdown
							dangerouslySetInnerHTML={{ __html: renderMarkdown(thread.body) }}
						/>
					)}
					{thread.replies.length > 0 && (
						<ul className="flex flex-col gap-2 px-4 py-3">
							{thread.replies.map((reply, ri) => (
								<li
									// Same reasoning as the thread key above.
									// eslint-disable-next-line react/no-array-index-key
									key={`reply-${idx}-${ri}`}
									className="border-l-2 border-neutral-200 pl-3"
									style={{ marginLeft: `${(reply.depth - 1) * 1.25}rem` }}
								>
									<div
										className="prose prose-sm max-w-none"
										// biome-ignore lint/security/noDangerouslySetInnerHtml: rendered + sanitised through renderMarkdown
										dangerouslySetInnerHTML={{
											__html: renderMarkdown(reply.body),
										}}
									/>
								</li>
							))}
						</ul>
					)}
				</section>
			))}
		</div>
	);
}
