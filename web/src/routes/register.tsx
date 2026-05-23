import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import toast from "react-hot-toast";
import { Captcha } from "../components/Captcha";
import { ApiError, register } from "../lib/api";
import { type CaptchaFrontendConfig, fetchCaptchaConfig } from "../lib/captcha";

/**
 * Account registration page.
 *
 * Fetches the operator's CAPTCHA config on mount and mounts the widget
 * (today: hCaptcha) when the server published one. Submits to
 * `POST /api/v1/auth/register` with the verified token attached.
 *
 * The actual create-user flow is gated by `auth.registration` on the
 * server (#13) — `"closed"` (the default) surfaces as a 403, which the
 * page reports as "Registration is disabled on this wiki".
 */
export const Route = createFileRoute("/register")({
	component: RegisterComponent,
});

function RegisterComponent() {
	const navigate = useNavigate();
	const [username, setUsername] = useState("");
	const [password, setPassword] = useState("");
	const [email, setEmail] = useState("");
	const [displayName, setDisplayName] = useState("");
	const [captchaConfig, setCaptchaConfig] = useState<CaptchaFrontendConfig | null>(null);
	const [captchaConfigLoaded, setCaptchaConfigLoaded] = useState(false);
	const [captchaToken, setCaptchaToken] = useState<string | null>(null);
	const [submitting, setSubmitting] = useState(false);

	useEffect(() => {
		let cancelled = false;
		fetchCaptchaConfig().then((cfg) => {
			if (!cancelled) {
				setCaptchaConfig(cfg);
				setCaptchaConfigLoaded(true);
			}
		});
		return () => {
			cancelled = true;
		};
	}, []);

	const onSubmit = async (event: React.FormEvent<HTMLFormElement>) => {
		event.preventDefault();
		if (submitting) return;

		if (username.trim().length === 0) {
			toast.error("Username is required");
			return;
		}
		if (password.length === 0) {
			toast.error("Password is required");
			return;
		}
		if (captchaConfig && !captchaToken) {
			toast.error("Please complete the CAPTCHA challenge");
			return;
		}

		setSubmitting(true);
		try {
			const body: import("../lib/api").RegisterRequest = {
				username: username.trim(),
				password,
			};
			const trimmedEmail = email.trim();
			if (trimmedEmail.length > 0) {
				body.email = trimmedEmail;
			}
			const trimmedDisplay = displayName.trim();
			if (trimmedDisplay.length > 0) {
				body.display_name = trimmedDisplay;
			}
			if (captchaToken) {
				body.captcha_response = captchaToken;
			}
			await register(body);
			toast.success("Account created — please log in");
			navigate({ to: "/login" });
		} catch (err) {
			if (err instanceof ApiError) {
				if (err.status === 403) {
					toast.error("Registration is disabled on this wiki");
				} else if (err.status === 400) {
					toast.error(`Could not create account: ${err.message}`);
				} else if (err.status === 502) {
					toast.error("CAPTCHA service unreachable — try again later");
				} else {
					toast.error(`Registration failed: ${err.message}`);
				}
			} else {
				toast.error("Registration failed");
			}
		} finally {
			setSubmitting(false);
		}
	};

	return (
		<main className="mx-auto flex max-w-md flex-col gap-4 px-6 py-16">
			<header>
				<h1 className="text-2xl font-semibold tracking-tight">Register</h1>
				<p className="mt-1 text-sm text-neutral-600">
					Create an account. Subject to the operator's registration policy and the configured CAPTCHA.
				</p>
			</header>

			<form className="flex flex-col gap-3" onSubmit={onSubmit}>
				<label className="flex flex-col gap-1 text-sm">
					<span className="font-medium text-neutral-700">Username</span>
					<input
						type="text"
						value={username}
						onChange={(event) => setUsername(event.target.value)}
						className="rounded-md border border-neutral-300 bg-white px-3 py-2 font-mono text-sm focus:border-neutral-500 focus:outline-none"
						autoComplete="username"
						required
					/>
				</label>

				<label className="flex flex-col gap-1 text-sm">
					<span className="font-medium text-neutral-700">Password</span>
					<input
						type="password"
						value={password}
						onChange={(event) => setPassword(event.target.value)}
						className="rounded-md border border-neutral-300 bg-white px-3 py-2 font-mono text-sm focus:border-neutral-500 focus:outline-none"
						autoComplete="new-password"
						required
					/>
				</label>

				<label className="flex flex-col gap-1 text-sm">
					<span className="font-medium text-neutral-700">Email (optional)</span>
					<input
						type="email"
						value={email}
						onChange={(event) => setEmail(event.target.value)}
						className="rounded-md border border-neutral-300 bg-white px-3 py-2 text-sm focus:border-neutral-500 focus:outline-none"
						autoComplete="email"
					/>
				</label>

				<label className="flex flex-col gap-1 text-sm">
					<span className="font-medium text-neutral-700">Display name (optional)</span>
					<input
						type="text"
						value={displayName}
						onChange={(event) => setDisplayName(event.target.value)}
						className="rounded-md border border-neutral-300 bg-white px-3 py-2 text-sm focus:border-neutral-500 focus:outline-none"
						autoComplete="name"
					/>
				</label>

				{captchaConfigLoaded && captchaConfig && (
					<Captcha
						config={captchaConfig}
						onVerify={(token) => setCaptchaToken(token)}
						onExpire={() => setCaptchaToken(null)}
					/>
				)}

				<button
					type="submit"
					disabled={submitting}
					className="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-neutral-800 disabled:cursor-not-allowed disabled:opacity-60"
				>
					{submitting ? "Creating account…" : "Create account"}
				</button>
			</form>
		</main>
	);
}
