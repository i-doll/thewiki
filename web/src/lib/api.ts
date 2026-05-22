//! Typed wrappers around the page CRUD endpoints in `crates/api/src/pages`.
//!
//! Mutating requests inject the placeholder `X-User-Id` header (see
//! `lib/auth.ts`). Read endpoints don't carry the header — the API only
//! requires auth on writes today.

import { getCurrentUserId } from "./auth";

/**
 * Per-page protection level mirroring `thewiki_core::ProtectionLevel`.
 * Snake-case to match the serde wire form.
 */
export type ProtectionLevel = "none" | "semi_protected" | "protected" | "fully_protected";

/** Mirrors `PageView` from `crates/api/src/pages/dto.rs`. */
export interface PageView {
	id: string;
	namespace_id: string;
	namespace_slug: string;
	slug: string;
	title: string;
	current_revision_id: string | null;
	content: string;
	protection_level: ProtectionLevel;
	created_at: string;
	updated_at: string;
}

/** Mirrors `PageListItem`. */
export interface PageListItem {
	id: string;
	namespace_slug: string;
	slug: string;
	title: string;
	updated_at: string;
}

/** Mirrors `PageListResponse`. */
export interface PageListResponse {
	items: PageListItem[];
	next_cursor: string | null;
}

/** Body for `POST /api/v1/pages`. */
export interface CreatePageRequest {
	namespace_slug: string;
	slug: string;
	title: string;
	content: string;
}

/** Body for `PUT /api/v1/pages/{slug}`. */
export interface UpdatePageRequest {
	title?: string;
	content: string;
	edit_summary?: string;
}

/**
 * Error thrown by the API helpers. `status === 404` is the signal the route
 * components use to switch to the "page not found" view; other statuses bubble
 * up as generic failures.
 */
export class ApiError extends Error {
	readonly status: number;

	constructor(status: number, message: string) {
		super(message);
		this.name = "ApiError";
		this.status = status;
	}
}

interface ErrorBody {
	error?: { message?: string };
	message?: string;
}

async function parseError(res: Response): Promise<string> {
	try {
		const body = (await res.json()) as ErrorBody;
		return body.error?.message ?? body.message ?? `${res.status} ${res.statusText}`;
	} catch {
		return `${res.status} ${res.statusText}`;
	}
}

async function jsonRequest<T>(input: string, init: RequestInit): Promise<T> {
	const res = await fetch(input, init);
	if (!res.ok) {
		const message = await parseError(res);
		throw new ApiError(res.status, message);
	}
	return (await res.json()) as T;
}

function authHeaders(): HeadersInit {
	return {
		"content-type": "application/json",
		"x-user-id": getCurrentUserId(),
	};
}

export async function fetchPage(slug: string): Promise<PageView> {
	return jsonRequest<PageView>(`/api/v1/pages/${encodeURIComponent(slug)}`, {
		method: "GET",
	});
}

export async function listPages(options: {
	cursor?: string | null;
	limit?: number;
}): Promise<PageListResponse> {
	const params = new URLSearchParams();
	if (options.cursor) {
		params.set("cursor", options.cursor);
	}
	if (options.limit !== undefined) {
		params.set("limit", String(options.limit));
	}
	const query = params.toString();
	const url = query.length > 0 ? `/api/v1/pages?${query}` : "/api/v1/pages";
	return jsonRequest<PageListResponse>(url, { method: "GET" });
}

export async function createPage(body: CreatePageRequest): Promise<PageView> {
	return jsonRequest<PageView>("/api/v1/pages", {
		method: "POST",
		headers: authHeaders(),
		body: JSON.stringify(body),
	});
}

export async function updatePage(slug: string, body: UpdatePageRequest): Promise<PageView> {
	return jsonRequest<PageView>(`/api/v1/pages/${encodeURIComponent(slug)}`, {
		method: "PUT",
		headers: authHeaders(),
		body: JSON.stringify(body),
	});
}

/**
 * Body for `POST /api/v1/pages/{slug}/protect` (#34).
 */
export interface ProtectPageRequest {
	protection_level: ProtectionLevel;
}

/**
 * Change a page's protection level. Requires the `PROTECT` permission on
 * the calling session — the server returns a `page_protected` 403 otherwise,
 * which surfaces in the SPA via [`ApiError`].
 */
export async function protectPage(slug: string, body: ProtectPageRequest): Promise<PageView> {
	return jsonRequest<PageView>(`/api/v1/pages/${encodeURIComponent(slug)}/protect`, {
		method: "POST",
		headers: authHeaders(),
		body: JSON.stringify(body),
	});
}

/**
 * Payload returned by `GET /api/v1/auth/me`. Permissions are the
 * pipe-separated flag string emitted by the API (`"READ | EDIT"`), so the
 * SPA can check membership with a plain split.
 */
export interface AuthMePayload {
	id: string;
	username: string;
	display_name: string | null;
	email: string | null;
	roles: string[];
	permissions: string;
}

/**
 * Fetch the calling user's profile, if any. Returns `null` on a 401 so
 * components can branch on "logged in?" without try/catching.
 *
 * Used by the view route to decide whether to show the Edit button, disable
 * it with a tooltip, or hide it entirely — see #34 acceptance criteria.
 */
export async function fetchAuthMe(): Promise<AuthMePayload | null> {
	const res = await fetch("/api/v1/auth/me", { method: "GET", credentials: "same-origin" });
	if (res.status === 401) {
		return null;
	}
	if (!res.ok) {
		const message = await parseError(res);
		throw new ApiError(res.status, message);
	}
	return (await res.json()) as AuthMePayload;
}

/**
 * Parse the `"READ | EDIT"` permissions string into a Set for ergonomic
 * `has(...)` checks.
 */
export function parsePermissions(raw: string | undefined | null): Set<string> {
	if (!raw) {
		return new Set();
	}
	return new Set(
		raw
			.split("|")
			.map((s) => s.trim())
			.filter((s) => s.length > 0),
	);
}

/**
 * Compute whether the supplied permission set is allowed to mutate a page
 * at the given protection level. Mirrors `check_protection` in
 * `crates/api/src/pages/protection.rs`; SPA logic only — server still
 * enforces the same matrix on every request.
 */
export function canEditAtProtectionLevel(
	level: ProtectionLevel,
	authenticated: boolean,
	permissions: Set<string>,
): boolean {
	switch (level) {
		case "none":
			return true;
		case "semi_protected":
			return authenticated;
		case "protected":
			return permissions.has("EDIT");
		case "fully_protected":
			return permissions.has("PROTECT");
	}
}
