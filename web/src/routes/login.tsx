import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import toast from "react-hot-toast";
import {
	clearCurrentUserId,
	DEFAULT_DEV_USER_ID,
	getCurrentUserId,
	setCurrentUserId,
} from "../lib/auth";

/**
 * Temporary login page.
 *
 * The real auth flow (session cookies + Argon2) is tracked by #14. Until that
 * lands, mutating requests carry a fixed `X-User-Id` header read from
 * `localStorage`. This page lets a dev or test user swap that id without
 * opening DevTools.
 */
export const Route = createFileRoute("/login")({
	component: LoginComponent,
});

function LoginComponent() {
	const navigate = useNavigate();
	const [userId, setUserId] = useState<string>(() => getCurrentUserId());

	const onSave = () => {
		const trimmed = userId.trim();
		if (trimmed.length === 0) {
			toast.error("User id must not be empty");
			return;
		}
		setCurrentUserId(trimmed);
		toast.success("Saved user id");
		navigate({ to: "/wiki" });
	};

	const onReset = () => {
		clearCurrentUserId();
		setUserId(DEFAULT_DEV_USER_ID);
		toast.success("Reset to dev default");
	};

	return (
		<main className="mx-auto flex max-w-md flex-col gap-4 px-6 py-16">
			<header>
				<h1 className="text-2xl font-semibold tracking-tight">Login</h1>
				<p className="mt-1 text-sm text-neutral-600">
					Temporary shim until session cookies land (#14). The user id you set here is sent as the{" "}
					<code className="rounded bg-neutral-200 px-1 font-mono text-xs">X-User-Id</code> header on
					every mutating request.
				</p>
			</header>

			<label className="flex flex-col gap-1 text-sm">
				<span className="font-medium text-neutral-700">User id (UUID)</span>
				<input
					type="text"
					value={userId}
					onChange={(event) => setUserId(event.target.value)}
					className="rounded-md border border-neutral-300 bg-white px-3 py-2 font-mono text-sm focus:border-neutral-500 focus:outline-none"
					placeholder={DEFAULT_DEV_USER_ID}
				/>
			</label>

			<div className="flex gap-3">
				<button
					type="button"
					onClick={onSave}
					className="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-neutral-800"
				>
					Save
				</button>
				<button
					type="button"
					onClick={onReset}
					className="rounded-md border border-neutral-300 bg-white px-3 py-1.5 text-sm font-medium text-neutral-800 hover:bg-neutral-100"
				>
					Reset to dev default
				</button>
			</div>
		</main>
	);
}
