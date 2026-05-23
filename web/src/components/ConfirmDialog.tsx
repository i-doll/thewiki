//! Small unstyled confirmation modal used by every destructive admin
//! action (#47). Cancel is the default focused button so a user can press
//! `Enter` straight away to back out — the issue explicitly calls out this
//! shape.
//!
//! Intentionally not a full design-system primitive: the codebase has no
//! `<Dialog />` yet and pulling in a UI library would dwarf the feature.
//! The element is built on `<dialog>` so the browser handles focus
//! trapping and the `Esc`-to-cancel keybind. Tailwind handles the
//! presentation so the markup matches the rest of the SPA.

import { useEffect, useRef } from "react";

export interface ConfirmDialogProps {
	/** Whether the dialog is currently visible. */
	open: boolean;
	/** Title rendered at the top of the dialog. */
	title: string;
	/** Plain-English summary of what the action does. */
	message: React.ReactNode;
	/** Label for the destructive button. Defaults to "Confirm". */
	confirmLabel?: string;
	/** Label for the cancel button. Defaults to "Cancel". */
	cancelLabel?: string;
	/** Disable both buttons (e.g. while the mutation is in flight). */
	busy?: boolean;
	/**
	 * Optional supplemental content rendered between the message and the
	 * action buttons. Used by the role-delete dialog to surface a
	 * "View affected users" deep-link when the role is still assigned.
	 */
	footer?: React.ReactNode;
	/** Called when the user clicks `Confirm`. */
	onConfirm: () => void;
	/** Called when the user clicks `Cancel`, presses Esc, or backdrop-clicks. */
	onCancel: () => void;
}

export function ConfirmDialog({
	open,
	title,
	message,
	confirmLabel = "Confirm",
	cancelLabel = "Cancel",
	busy,
	footer,
	onConfirm,
	onCancel,
}: ConfirmDialogProps) {
	const dialogRef = useRef<HTMLDialogElement>(null);
	const cancelRef = useRef<HTMLButtonElement>(null);

	// Drive the native <dialog> open/close lifecycle. `showModal()` is what
	// gives us the backdrop + focus trap; we only call it when transitioning
	// from closed → open so React renders match the imperative state.
	useEffect(() => {
		const node = dialogRef.current;
		if (!node) return;
		if (open && !node.open) {
			node.showModal();
			// Focus the cancel button so Enter defaults to "back out". The
			// confirm button stays a deliberate, mouse-driven action.
			cancelRef.current?.focus();
		} else if (!open && node.open) {
			node.close();
		}
	}, [open]);

	// When the browser fires `cancel` (Esc or click outside) propagate it
	// back to the parent so React state stays in sync with the DOM state.
	useEffect(() => {
		const node = dialogRef.current;
		if (!node) return;
		const handler = (event: Event) => {
			event.preventDefault();
			onCancel();
		};
		node.addEventListener("cancel", handler);
		return () => node.removeEventListener("cancel", handler);
	}, [onCancel]);

	return (
		<dialog
			ref={dialogRef}
			className="rounded-md border border-neutral-200 bg-white p-0 shadow-lg backdrop:bg-neutral-900/40"
			// biome-ignore lint/a11y/useKeyWithClickEvents: backdrop click is paired with the native dialog's Esc key handler wired above.
			onClick={(e) => {
				// Backdrop click — the click hits the <dialog> itself, not a child.
				if (e.target === dialogRef.current) {
					onCancel();
				}
			}}
		>
			<div className="w-[min(28rem,calc(100vw-2rem))] p-6">
				<h2 className="mb-2 text-lg font-semibold text-neutral-900">{title}</h2>
				<div className="mb-4 text-sm text-neutral-700">{message}</div>
				{footer && <div className="mb-4 text-sm">{footer}</div>}
				<div className="flex justify-end gap-2">
					<button
						type="button"
						ref={cancelRef}
						onClick={onCancel}
						disabled={busy}
						className="rounded border border-neutral-300 bg-white px-4 py-2 text-sm font-medium text-neutral-700 hover:bg-neutral-50 disabled:opacity-50"
					>
						{cancelLabel}
					</button>
					<button
						type="button"
						onClick={onConfirm}
						disabled={busy}
						className="rounded bg-red-600 px-4 py-2 text-sm font-medium text-white hover:bg-red-700 disabled:opacity-50"
					>
						{confirmLabel}
					</button>
				</div>
			</div>
		</dialog>
	);
}
