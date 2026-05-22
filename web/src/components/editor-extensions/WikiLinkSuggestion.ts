//! Tiptap suggestion plugin that triggers on `[[` and surfaces the shared
//! `WikiLinkAutocomplete` dropdown.
//!
//! The plugin itself only owns the "when to fire" + "how to replace text"
//! logic. UI rendering is delegated to a React callback supplied at
//! construction time (`onStateChange`) — that lets us mount a single dropdown
//! at the editor-wrapper level rather than one per editor mode, and keeps the
//! TanStack Query cache lookups (which need to live in React land) outside of
//! this plain TypeScript module.
//!
//! Trigger mechanics: `@tiptap/suggestion` only accepts a single trigger
//! character. We pass `char: '['` plus `allowedPrefixes: ['[']`, which fires
//! the plugin exactly when a `[` is typed *immediately after* another `[`. The
//! plugin then captures everything after the second `[` as the query until
//! whitespace or `]` (the latter via `allowToIncludeChar: false`).

import { Extension, type Range } from "@tiptap/core";
import Suggestion from "@tiptap/suggestion";

/**
 * Snapshot of suggestion state pushed to the React layer. Whenever Tiptap
 * starts / updates / exits the suggestion, the host editor receives one of
 * these — the consumer is responsible for mounting or unmounting the dropdown
 * accordingly.
 */
export interface WikiLinkSuggestionState {
	/** Current substring typed after `[[`. May be empty just after triggering. */
	query: string;
	/** Document-relative range covering the trigger and the query so far. */
	range: Range;
	/** Returns the caret's viewport-relative rect; null if the editor is hidden. */
	clientRect: (() => DOMRect | null) | null;
	/**
	 * Replace `[[query` (the matched range) with `[[target]]`. Called by the
	 * dropdown when the user picks a hit or commits the redlink CTA.
	 */
	command: (target: string) => void;
}

export interface WikiLinkSuggestionOptions {
	/**
	 * Notified every time the suggestion starts, updates, or exits. The host
	 * editor uses this to mount/update/unmount the autocomplete dropdown.
	 *
	 * `state === null` is the exit signal.
	 */
	onStateChange: (state: WikiLinkSuggestionState | null) => void;
}

/**
 * Tiptap extension wrapping `@tiptap/suggestion` with the wiki-link policy.
 *
 * Usage:
 * ```ts
 * const ext = WikiLinkSuggestion.configure({
 *   onStateChange: (state) => setSuggestion(state),
 * });
 * ```
 */
export const WikiLinkSuggestion = Extension.create<WikiLinkSuggestionOptions>({
	name: "wikiLinkSuggestion",

	addOptions() {
		return {
			// Default to a no-op so misconfiguration only causes the UI not to
			// appear — never an unhandled throw inside the editor pipeline.
			onStateChange: () => {},
		};
	},

	addProseMirrorPlugins() {
		const { onStateChange } = this.options;

		return [
			Suggestion({
				editor: this.editor,
				char: "[",
				// Require the previous character to be `[` so the plugin only fires
				// on the *second* `[` of `[[`.
				allowedPrefixes: ["["],
				allowSpaces: true,
				// Don't include `[` in the query — `[[[Foo` shouldn't smuggle the
				// extra bracket into the search text.
				allowToIncludeChar: false,
				startOfLine: false,
				command: ({ editor, range, props }) => {
					// `props` is whatever the dropdown passed to `command(...)` —
					// here a `{ target: string }` payload. Apply the replacement
					// with the full matched range so the leading `[[` and the
					// in-progress query both get replaced together.
					const { target } = props as { target: string };
					const insert = `[[${target}]]`;
					editor.chain().focus().insertContentAt({ from: range.from, to: range.to }, insert).run();
				},
				render: () => {
					return {
						onStart: (props) => {
							onStateChange({
								query: props.query,
								range: props.range,
								clientRect: props.clientRect ?? null,
								// Delegate to the plugin's own `command` handler so the replacement uses
								// the live range (mapped through any intervening transactions)
								// rather than the snapshot we captured at start time.
								command: (target: string) => props.command({ target }),
							});
						},
						onUpdate: (props) => {
							onStateChange({
								query: props.query,
								range: props.range,
								clientRect: props.clientRect ?? null,
								// Delegate to the plugin's own `command` handler so the replacement uses
								// the live range (mapped through any intervening transactions)
								// rather than the snapshot we captured at start time.
								command: (target: string) => props.command({ target }),
							});
						},
						onKeyDown: ({ event }) => {
							// Forward the navigation/dismissal keys to the React-side
							// dropdown by *not* handling them here. Returning `true`
							// would tell ProseMirror we consumed the event and prevent
							// the dropdown's document-level listener from seeing it.
							//
							// We still need to stop Enter from inserting a newline in
							// the editor while the dropdown is visible — the dropdown
							// owns Enter for accepting the selection.
							if (event.key === "Enter") {
								return true;
							}
							if (event.key === "Escape") {
								return true;
							}
							if (event.key === "ArrowUp" || event.key === "ArrowDown") {
								return true;
							}
							return false;
						},
						onExit: () => {
							onStateChange(null);
						},
					};
				},
			}),
		];
	},
});

export default WikiLinkSuggestion;
