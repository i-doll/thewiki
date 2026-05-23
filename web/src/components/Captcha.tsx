/**
 * CAPTCHA mount point (#41).
 *
 * Renders the hCaptcha widget when the server published a matching
 * frontend config, otherwise renders nothing. The parent form gets the
 * resulting token through `onVerify` and is responsible for sending it
 * back along with the form submission.
 *
 * Implementation notes:
 * - We inject the hCaptcha script lazily on mount via
 *   `ensureHcaptchaScript()`. Multiple Captcha components on the same
 *   page share a single script tag.
 * - We poll for `window.hcaptcha` briefly because the script is
 *   `async defer` and may not be ready the first tick after mount. Once
 *   it lands, we call `window.hcaptcha.render(...)` against the div ref.
 * - On unmount we leave the script tag in place (it's idempotent) and
 *   tell hCaptcha to release the widget if it was rendered.
 * - The parent gets an imperative handle (`resetCaptcha()`) via `ref` so
 *   it can force a re-solve when a submit fails after the token was
 *   already burned. hCaptcha tokens are single-use; without this the next
 *   submit replays the dead token and the API rejects it with
 *   `captcha_failed` (no UX feedback that the user has to re-solve).
 */

import { forwardRef, useEffect, useImperativeHandle, useRef, useState } from "react";
import { type CaptchaFrontendConfig, ensureHcaptchaScript } from "../lib/captcha";

interface CaptchaProps {
	config: CaptchaFrontendConfig;
	/** Called with the verified token once the user solves the challenge. */
	onVerify: (token: string) => void;
	/** Called when the token expires or is invalidated. */
	onExpire?: () => void;
}

/**
 * Imperative handle the parent form holds via `ref`. The only operation
 * we expose is `resetCaptcha()` — used by the register page when an API
 * submit fails (e.g. username conflict) so the now-burned token isn't
 * replayed on the next attempt.
 */
export interface CaptchaHandle {
	/** Reset the widget so the user can solve a fresh challenge. */
	resetCaptcha: () => void;
}

// biome-ignore lint/suspicious/noExplicitAny: window.hcaptcha is an external global
type HCaptchaGlobal = any;

/**
 * Read the hCaptcha global off the window, returning `null` when the
 * script hasn't loaded yet.
 */
function hcaptchaGlobal(): HCaptchaGlobal | null {
	if (typeof window === "undefined") {
		return null;
	}
	// biome-ignore lint/suspicious/noExplicitAny: external global
	return (window as any).hcaptcha ?? null;
}

export const Captcha = forwardRef<CaptchaHandle, CaptchaProps>(function Captcha(
	{ config, onVerify, onExpire },
	ref,
) {
	const containerRef = useRef<HTMLDivElement | null>(null);
	const widgetIdRef = useRef<string | number | null>(null);
	const [ready, setReady] = useState<boolean>(() => hcaptchaGlobal() !== null);

	// Expose `resetCaptcha()` to the parent so a failed submit can clear
	// the burned token. Safe to call before the widget has rendered — we
	// just no-op if the global isn't there yet.
	useImperativeHandle(
		ref,
		() => ({
			resetCaptcha: () => {
				const hcaptcha = hcaptchaGlobal();
				const id = widgetIdRef.current;
				if (hcaptcha?.reset && id !== null) {
					try {
						hcaptcha.reset(id);
					} catch {
						// noop: best-effort reset; if hCaptcha throws we
						// surface the failure as a stale-token submission
						// rather than crashing the page.
					}
				}
			},
		}),
		[],
	);

	// Inject the script tag once on first mount across the page.
	useEffect(() => {
		if (config.provider !== "hcaptcha") {
			return;
		}
		ensureHcaptchaScript();
		if (hcaptchaGlobal()) {
			setReady(true);
			return;
		}
		// Poll briefly until the script self-installs `window.hcaptcha`.
		// 200ms intervals up to ~10s is generous; in practice this resolves
		// in a couple of ticks. We give up silently after the budget — the
		// server still enforces the gate, so the worst case is a form the
		// user can't submit, which is the right failure mode.
		const interval = window.setInterval(() => {
			if (hcaptchaGlobal()) {
				setReady(true);
				window.clearInterval(interval);
			}
		}, 200);
		const timeout = window.setTimeout(() => {
			window.clearInterval(interval);
		}, 10_000);
		return () => {
			window.clearInterval(interval);
			window.clearTimeout(timeout);
		};
	}, [config.provider]);

	// Render the widget once the global is present and the container is mounted.
	useEffect(() => {
		if (!ready) return;
		if (config.provider !== "hcaptcha") return;
		const hcaptcha = hcaptchaGlobal();
		if (!hcaptcha || !containerRef.current) return;
		// Avoid double-rendering on re-runs of the effect (StrictMode, etc.).
		if (widgetIdRef.current !== null) return;

		try {
			widgetIdRef.current = hcaptcha.render(containerRef.current, {
				sitekey: config.site_key,
				callback: (token: string) => onVerify(token),
				"expired-callback": () => {
					if (onExpire) onExpire();
				},
				"error-callback": () => {
					if (onExpire) onExpire();
				},
			});
		} catch (err) {
			// hcaptcha.render throws when the container has already been
			// initialised by a previous mount that StrictMode discarded.
			// We log to the console (devtools-only) and let the existing
			// widget keep working.
			console.warn("hcaptcha render failed", err);
		}

		return () => {
			const id = widgetIdRef.current;
			if (id !== null && hcaptcha?.reset) {
				try {
					hcaptcha.reset(id);
				} catch {
					// noop: best-effort cleanup
				}
			}
			widgetIdRef.current = null;
		};
	}, [ready, config.provider, config.site_key, onVerify, onExpire]);

	if (config.provider !== "hcaptcha") {
		// Future providers (Turnstile, reCAPTCHA, …) can be wired in here
		// without affecting the parent form.
		return null;
	}

	return <div ref={containerRef} className="my-3" />;
});
