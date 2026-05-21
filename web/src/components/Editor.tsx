import { markdown } from "@codemirror/lang-markdown";
import { oneDark } from "@codemirror/theme-one-dark";
import type { Command, KeyBinding } from "@codemirror/view";
import { keymap } from "@codemirror/view";
import { Extension } from "@tiptap/core";
import { EditorContent, useEditor } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import CodeMirror from "@uiw/react-codemirror";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Markdown } from "tiptap-markdown";

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
 * selection is empty, places the cursor inside the empty link text.
 */
const insertLink: Command = (view) => {
	const { state } = view;
	const changes = state.changeByRange((range) => {
		const selected = state.sliceDoc(range.from, range.to);
		const replacement = `[${selected}](url)`;
		// Place the cursor inside the URL placeholder so the user can type it.
		const urlStart = range.from + 1 + selected.length + 2; // after `[selected](`
		const cursor = range.empty ? range.from + 1 : urlStart;
		return {
			changes: { from: range.from, to: range.to, insert: replacement },
			range: { anchor: cursor, head: cursor + (range.empty ? 0 : 3) } as never,
		};
	});
	view.dispatch(state.update(changes, { scrollIntoView: true, userEvent: "input" }));
	return true;
};

const markdownKeymap: KeyBinding[] = [
	{ key: "Mod-b", run: buildWrapKeymap("**", "**") },
	{ key: "Mod-i", run: buildWrapKeymap("*", "*") },
	{ key: "Mod-k", run: insertLink },
];

interface TiptapEditorProps {
	markdown: string;
	onChange: (next: string) => void;
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

function TiptapEditor({ markdown: markdownValue, onChange }: TiptapEditorProps) {
	// Track the last markdown we *emitted* so we don't loop on our own updates.
	const lastEmittedRef = useRef<string>(markdownValue);

	const editor = useEditor({
		extensions: [
			StarterKit,
			LinkShortcut,
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
}

function SourceEditor({ value, onChange }: SourceEditorProps) {
	const extensions = useMemo(() => [markdown(), keymap.of(markdownKeymap)], []);

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
				<TiptapEditor markdown={value} onChange={onChange} />
			) : (
				<SourceEditor value={value} onChange={onChange} />
			)}
		</div>
	);
}

export default Editor;
