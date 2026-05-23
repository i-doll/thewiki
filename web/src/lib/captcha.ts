//! CAPTCHA wiring for the SPA (#41).
//!
//! The server publishes the public widget config at
//! `GET /api/v1/captcha/config`. A `null` response means no widget should
//! be rendered (the noop provider is wired, or no provider at all). An
//! object with `provider` + `site_key` tells the SPA which embed to mount
//! and what public key to hand it.
//!
//! This module also injects the hCaptcha script tag on demand, guarded by
//! a `useEffect` that checks `window.hcaptcha` so we don't load the same
//! external script twice if multiple forms render the widget on the same
//! page.

/** Mirrors `CaptchaFrontendConfig` from `crates/core/src/captcha.rs`. */
export interface CaptchaFrontendConfig {
	provider: string;
	site_key: string;
}

/**
 * Fetch the operator-published CAPTCHA config. Returns `null` when the
 * server didn't wire a widget-rendering provider — the form should then
 * skip the challenge.
 *
 * Failure modes (network blip, 5xx, malformed body) fall through as
 * `null` rather than throwing: the SPA can still render the form, and the
 * server will reject the submission if the gate is actually required.
 */
export async function fetchCaptchaConfig(): Promise<CaptchaFrontendConfig | null> {
	try {
		const res = await fetch("/api/v1/captcha/config", {
			method: "GET",
			credentials: "same-origin",
		});
		if (!res.ok) {
			return null;
		}
		const body = (await res.json()) as CaptchaFrontendConfig | null;
		if (body === null || typeof body !== "object") {
			return null;
		}
		if (typeof body.provider !== "string" || typeof body.site_key !== "string") {
			return null;
		}
		return body;
	} catch {
		return null;
	}
}

/** URL of the official hCaptcha embed script. */
export const HCAPTCHA_SCRIPT_URL = "https://js.hcaptcha.com/1/api.js";

/**
 * Inject the hCaptcha script tag once per document. Subsequent calls are
 * a no-op: the script self-registers `window.hcaptcha` on load, and once
 * the global is present we don't need to load it again.
 *
 * The script is added with `async defer` so it never blocks paint; the
 * widget renders as soon as the script's onload fires (which the embed
 * library handles via its own MutationObserver).
 */
export function ensureHcaptchaScript(): void {
	if (typeof window === "undefined") {
		// SSR: nothing to do. (TanStack Start does run server-side, but
		// captcha mounting is strictly a client concern.)
		return;
	}
	// biome-ignore lint/suspicious/noExplicitAny: window.hcaptcha is an external global
	if ((window as any).hcaptcha) {
		return;
	}
	const existing = document.querySelector<HTMLScriptElement>(
		`script[src="${HCAPTCHA_SCRIPT_URL}"]`,
	);
	if (existing) {
		return;
	}
	const script = document.createElement("script");
	script.src = HCAPTCHA_SCRIPT_URL;
	script.async = true;
	script.defer = true;
	document.head.appendChild(script);
}
