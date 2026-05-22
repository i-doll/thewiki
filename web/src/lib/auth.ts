//! Temporary client-side auth shim.
//!
//! Until proper session-cookie auth lands (#14), mutating API calls must carry
//! an `X-User-Id` header (see `crates/api/src/extractors.rs`). We persist the
//! caller's user id in `localStorage` so refreshes don't lose it, and fall back
//! to a hardcoded dev UUID so a fresh checkout can still create pages without
//! requiring the user to visit `/login` first.
//!
//! TODO(#14): delete this file once session cookies replace the header.

const STORAGE_KEY = "thewiki:user-id";

/**
 * Default user id used when nothing is stored in `localStorage`. The matching
 * row is seeded by `dev.toml` / the dev fixtures so requests don't 404 against
 * a missing user. Production deploys must replace this whole shim with real
 * session auth before going live (see #14).
 */
export const DEFAULT_DEV_USER_ID = "00000000-0000-0000-0000-000000000001";

export function getCurrentUserId(): string {
	if (typeof window === "undefined") {
		return DEFAULT_DEV_USER_ID;
	}
	try {
		const stored = window.localStorage.getItem(STORAGE_KEY);
		if (stored && stored.trim().length > 0) {
			return stored;
		}
	} catch {
		// localStorage may be disabled (private mode, etc.). Fall through.
	}
	return DEFAULT_DEV_USER_ID;
}

export function setCurrentUserId(userId: string): void {
	if (typeof window === "undefined") {
		return;
	}
	try {
		window.localStorage.setItem(STORAGE_KEY, userId);
	} catch {
		// Best-effort — surface a console warning so devs notice the failure
		// without making the UI explode.
		// eslint-disable-next-line no-console
		console.warn("Failed to persist thewiki:user-id to localStorage");
	}
}

export function clearCurrentUserId(): void {
	if (typeof window === "undefined") {
		return;
	}
	try {
		window.localStorage.removeItem(STORAGE_KEY);
	} catch {
		// Ignore — best effort.
	}
}
