import { autocompletion, closeCompletion } from "@codemirror/autocomplete";
import { markdown } from "@codemirror/lang-markdown";
import { oneDark } from "@codemirror/theme-one-dark";
import type { Command, KeyBinding } from "@codemirror/view";
import { keymap } from "@codemirror/view";
import { useQueryClient } from "@tanstack/react-query";
import { Extension } from "@tiptap/core";
import { EditorContent, useEditor } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import CodeMirror from "@uiw/react-codemirror";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Markdown } from "tiptap-markdown";
import {
	WikiLinkSuggestion,
	type WikiLinkSuggestionState,
} from "./editor-extensions/WikiLinkSuggestion";
import { buildWikiLinkCompletion } from "./editor-extensions/wikiLinkCompletion";
import { WikiLinkAutocomplete } from "./WikiLinkAutocomplete";

export type EditorMode = "wysiwyg" | "source";

export interface EditorProps {
	value: string;
	onChange: (next: string) => void;
	initialMode?: EditorMode;
}

const STORAGE_KEY = "thewiki:editor-mode";

function readStoredMode(): EditorMode | null {
	if (typeof window === "undefined") {
		return null;
	}
	try {
		const raw = window.localStorage.getItem(STORAGE_KEY);
		if (raw === "wysiwyg" || raw === "source") {
			return raw;
		}
	} catch {
		// localStorage may throw (private mode, disabled, etc.). Fall through.
	}
	return null;
}

function writeStoredMode(mode: EditorMode): void {
	if (typeof window === "undefined") {
		return;
	}
	try {
		window.localStorage.setItem(STORAGE_KEY, mode);
	} catch {
		// Ignore storage failures — preference is best-effort.
	}
}

/**
 * Wraps the current selection with the given prefix/suffix. If the selection
 * is empty, inserts the markers and places the cursor between them.
 */
function buildWrapKeymap(prefix: string, suffix: string): Command {
	return (view) => {
		const { state } = view;
		const changes = state.changeByRange((range) => {
			const selected = state.sliceDoc(range.from, range.to);
			const replacement = `${prefix}${selected}${suffix}`;
			const cursor = range.empty ? range.from + prefix.length : range.from + replacement.length;
			return {
				changes: { from: range.from, to: range.to, insert: replacement },
				range: { anchor: cursor, head: cursor } as never,
			};
		});
		view.dispatch(state.update(changes, { scrollIntoView: true, userEvent: "input" }));
		return true;
	};
}

/**
 * Inserts a Markdown link around the current selection: `[text](url)`. If the
 * selection is empty, places the cursor inside the empty link text. Otherwise
 * selects the `url` placeholder so the user can type the URL straight away.
 */
const insertLink: Command = (view) => {
	const { state } = view;
	const changes = state.changeByRange((range) => {
		const selected = state.sliceDoc(range.from, range.to);
		const replacement = `[${selected}](url)`;
		// Position layout: `[selected](url)`
		//                   ^         ^   ^
		//                   from      |   end of replacement
		//                             urlStart (selects the literal "url")
		const urlStart = range.from + 1 + selected.length + 2;
		const cursor = range.empty ? range.from + 1 : urlStart;
		const head = range.empty ? cursor : urlStart + 3; // select exactly "url"
		return {
			changes: { from: range.from, to: range.to, insert: replacement },
			range: { anchor: cursor, head } as never,
		};
	});
	view.dispatch(state.update(changes, { scrollIntoView: true, userEvent: "input" }));
	return true;
};

function buildMarkdownKeymap(onToggleMode: () => void): KeyBinding[] {
	return [
		{ key: "Mod-b", run: buildWrapKeymap("**", "**") },
		{ key: "Mod-i", run: buildWrapKeymap("*", "*") },
		{ key: "Mod-k", run: insertLink },
		// Higher precedence than CodeMirror's default comment-toggle binding so
		// the global "switch editor mode" shortcut wins inside source mode.
		{
			key: "Mod-/",
			preventDefault: true,
			run: () => {
				onToggleMode();
				return true;
			},
		},
	];
}

interface TiptapEditorProps {
	markdown: string;
	onChange: (next: string) => void;
	onSuggestionChange: (state: WikiLinkSuggestionState | null) => void;
}

interface MarkdownStorageLike {
	getMarkdown?: () => string;
}

/**
 * Adds Cmd/Ctrl+K to Tiptap. The Link extension bundled in StarterKit ships
 * the `setLink` / `unsetLink` commands but no default keybinding. We prompt
 * for a URL and apply it to the current selection — matching the source-mode
 * behaviour of inserting a Markdown link.
 */
const LinkShortcut = Extension.create({
	name: "linkShortcut",
	addKeyboardShortcuts() {
		return {
			"Mod-k": () => {
				const { editor } = this;
				if (editor.state.selection.empty) {
					return false;
				}
				const previous = editor.getAttributes("link").href as string | undefined;
				const promptFn = typeof window !== "undefined" ? window.prompt : null;
				const url = promptFn ? promptFn("URL", previous ?? "https://") : null;
				if (url === null) {
					return true;
				}
				if (url === "") {
					return editor.chain().focus().extendMarkRange("link").unsetLink().run();
				}
				return editor.chain().focus().extendMarkRange("link").setLink({ href: url }).run();
			},
		};
	},
});

function TiptapEditor({
	markdown: markdownValue,
	onChange,
	onSuggestionChange,
}: TiptapEditorProps) {
	// Track the last markdown we *emitted* so we don't loop on our own updates.
	const lastEmittedRef = useRef<string>(markdownValue);

	// `useEditor` is recreated only on mount, so this ref keeps the suggestion
	// callback "live" even when the parent passes a new closure on every render.
	const suggestionCallbackRef = useRef(onSuggestionChange);
	useEffect(() => {
		suggestionCallbackRef.current = onSuggestionChange;
	}, [onSuggestionChange]);

	const editor = useEditor({
		// React 19 + StrictMode can render before the editor instance is ready;
		// `immediatelyRender: false` tells Tiptap to wait for the first effect
		// pass and avoids hydration-mismatch warnings.
		immediatelyRender: false,
		extensions: [
			StarterKit,
			LinkShortcut,
			WikiLinkSuggestion.configure({
				onStateChange: (state) => suggestionCallbackRef.current(state),
			}),
			Markdown.configure({
				html: false,
				tightLists: true,
				linkify: true,
				breaks: false,
				transformPastedText: true,
				transformCopiedText: true,
			}),
		],
		content: markdownValue,
		editorProps: {
			attributes: {
				class:
					"prose prose-sm max-w-none focus:outline-none min-h-[12rem] px-4 py-3 text-neutral-900",
			},
		},
		onUpdate: ({ editor: instance }) => {
			const storage = (instance.storage as unknown as Record<string, unknown>).markdown as
				| MarkdownStorageLike
				| undefined;
			const next = storage?.getMarkdown?.() ?? "";
			lastEmittedRef.current = next;
			onChange(next);
		},
	});

	// Sync external value changes (e.g. when the source-mode editor edits the
	// markdown and we swap back to WYSIWYG) into the Tiptap document.
	useEffect(() => {
		if (!editor) {
			return;
		}
		if (markdownValue === lastEmittedRef.current) {
			return;
		}
		lastEmittedRef.current = markdownValue;
		editor.commands.setContent(markdownValue, { emitUpdate: false });
	}, [editor, markdownValue]);

	return (
		<div className="rounded-md border border-neutral-200 bg-white">
			<EditorContent editor={editor} />
		</div>
	);
}

interface SourceEditorProps {
	value: string;
	onChange: (next: string) => void;
	onToggleMode: () => void;
}

function SourceEditor({ value, onChange, onToggleMode }: SourceEditorProps) {
	const queryClient = useQueryClient();

	// The keymap closes over `onToggleMode`, so rebuild it whenever the toggle
	// identity changes. Prepended via `Prec.highest`-style ordering: by passing
	// our keymap *after* the language extension, CodeMirror gives our bindings
	// priority over any defaults a future extension might register.
	//
	// The autocomplete extension is configured to show our wiki-link source
	// only — `defaultKeymap: true` keeps Up/Down/Enter/Escape bound to the
	// dropdown when it's open, which is exactly what we want.
	const extensions = useMemo(
		() => [
			markdown(),
			autocompletion({
				override: [buildWikiLinkCompletion({ queryClient })],
				// Keep the popup quiet until the user explicitly types `[[`.
				activateOnTyping: true,
				closeOnBlur: true,
				maxRenderedOptions: 6,
			}),
			keymap.of(buildMarkdownKeymap(onToggleMode)),
			// `closeCompletion` keeps the dropdown reactive to mode-switches
			// when the user toggles back to WYSIWYG mid-completion.
			keymap.of([{ key: "Escape", run: closeCompletion }]),
		],
		[onToggleMode, queryClient],
	);

	return (
		<CodeMirror
			value={value}
			onChange={onChange}
			extensions={extensions}
			theme={oneDark}
			basicSetup={{
				lineNumbers: true,
				highlightActiveLine: true,
				foldGutter: false,
			}}
			minHeight="12rem"
			className="overflow-hidden rounded-md border border-neutral-200 text-sm"
		/>
	);
}

export function Editor({ value, onChange, initialMode = "wysiwyg" }: EditorProps) {
	// Initialise from localStorage synchronously so the first render already shows
	// the persisted mode. `useState` initialiser runs once; safe for SSR because
	// `readStoredMode` returns `null` when `window` is unavailable.
	const [mode, setMode] = useState<EditorMode>(() => readStoredMode() ?? initialMode);

	// Tiptap-side wiki-link suggestion state. The CodeMirror path uses its own
	// dropdown (`@codemirror/autocomplete`), so this is only set when the
	// WYSIWYG editor is the active surface.
	const [suggestion, setSuggestion] = useState<WikiLinkSuggestionState | null>(null);

	// Recompute the dropdown's viewport anchor on every render — the suggestion
	// plugin gives us `clientRect`, but it's only meaningful at call time.
	const suggestionAnchor = useMemo(() => {
		if (!suggestion?.clientRect) {
			return null;
		}
		const rect = suggestion.clientRect();
		if (!rect) {
			return null;
		}
		// Offset a few px below the caret so the dropdown doesn't overlap the
		// text the user is typing.
		return { top: rect.bottom + 4, left: rect.left };
	}, [suggestion]);

	const toggleMode = useCallback(() => {
		setMode((current) => {
			const next: EditorMode = current === "wysiwyg" ? "source" : "wysiwyg";
			writeStoredMode(next);
			return next;
		});
	}, []);

	// Global Ctrl/Cmd+/ to toggle modes. We listen at the document level so the
	// shortcut works regardless of which editor currently has focus.
	useEffect(() => {
		const handler = (event: KeyboardEvent) => {
			if ((event.ctrlKey || event.metaKey) && event.key === "/") {
				event.preventDefault();
				toggleMode();
			}
		};
		window.addEventListener("keydown", handler);
		return () => {
			window.removeEventListener("keydown", handler);
		};
	}, [toggleMode]);

	return (
		<div className="flex flex-col gap-2">
			<div className="flex items-center justify-between">
				<fieldset className="inline-flex overflow-hidden rounded-md border border-neutral-300 p-0 text-xs">
					<legend className="sr-only">Editor mode</legend>
					<button
						type="button"
						onClick={() => {
							if (mode !== "wysiwyg") {
								toggleMode();
							}
						}}
						aria-pressed={mode === "wysiwyg"}
						className={
							mode === "wysiwyg"
								? "bg-neutral-900 px-3 py-1 font-medium text-white"
								: "bg-white px-3 py-1 text-neutral-700 hover:bg-neutral-100"
						}
					>
						WYSIWYG
					</button>
					<button
						type="button"
						onClick={() => {
							if (mode !== "source") {
								toggleMode();
							}
						}}
						aria-pressed={mode === "source"}
						className={
							mode === "source"
								? "bg-neutral-900 px-3 py-1 font-medium text-white"
								: "bg-white px-3 py-1 text-neutral-700 hover:bg-neutral-100"
						}
					>
						Source
					</button>
				</fieldset>
				<span className="text-xs text-neutral-500">
					<kbd className="rounded border border-neutral-300 bg-neutral-50 px-1 font-mono">
						Ctrl/Cmd
					</kbd>
					{" + "}
					<kbd className="rounded border border-neutral-300 bg-neutral-50 px-1 font-mono">/</kbd>
					{" to toggle"}
				</span>
			</div>

			{mode === "wysiwyg" ? (
				<TiptapEditor markdown={value} onChange={onChange} onSuggestionChange={setSuggestion} />
			) : (
				<SourceEditor value={value} onChange={onChange} onToggleMode={toggleMode} />
			)}

			{mode === "wysiwyg" && suggestion && suggestionAnchor && (
				<WikiLinkAutocomplete
					query={suggestion.query}
					position={suggestionAnchor}
					onSelect={(target) => {
						suggestion.command(target);
						setSuggestion(null);
					}}
					onClose={() => setSuggestion(null)}
				/>
			)}
		</div>
	);
}

export default Editor;
