//! Typed wrappers around the page CRUD endpoints in `crates/api/src/pages`.
//!
//! Mutating requests inject the placeholder `X-User-Id` header (see
//! `lib/auth.ts`). Read endpoints don't carry the header — the API only
//! requires auth on writes today.
//!
//! Namespace-aware routing (#28): the helpers in this module accept an
//! optional `namespace` argument that defaults to `Main`. When supplied
//! they call the namespace-aware API surface (`/api/v1/wiki/{namespace}/{slug}`);
//! otherwise they fall back to the legacy `/api/v1/pages/{slug}` routes
//! that assume `Main`. The legacy paths stay alive for back-compat and the
//! handful of consumers that haven't migrated yet.

import { getCurrentUserId } from "./auth";

/** Default namespace assumed when no explicit one is supplied. */
export const DEFAULT_NAMESPACE = "Main";

/**
 * Normalise an optional namespace into the slug to use on the wire.
 * Empty strings are treated as "absent" so URL parsing doesn't have to
 * coerce them.
 */
function resolveNamespace(namespace?: string | null): string {
	if (namespace === undefined || namespace === null || namespace.length === 0) {
		return DEFAULT_NAMESPACE;
	}
	return namespace;
}

/** Build the canonical `/api/v1/wiki/{namespace}/{slug}` URL for reads. */
function wikiPath(namespace: string | undefined | null, slug: string): string {
	const ns = encodeURIComponent(resolveNamespace(namespace));
	return `/api/v1/wiki/${ns}/${encodeURIComponent(slug)}`;
}

/**
 * Per-page protection level mirroring `thewiki_core::ProtectionLevel`.
 * Snake-case to match the serde wire form.
 */
export type ProtectionLevel = "none" | "semi_protected" | "protected" | "fully_protected";

/**
 * A category, as returned by the API.
 *
 * Mirrors `CategoryView` from `crates/api/src/categories/dto.rs` (#29).
 */
export interface CategoryView {
	id: string;
	slug: string;
	display_name: string;
	parent_id: string | null;
	created_at: string;
}

/** Mirrors `PageView` from `crates/api/src/pages/dto.rs`. */
export interface PageView {
	id: string;
	namespace_id: string;
	namespace_slug: string;
	slug: string;
	title: string;
	current_revision_id: string | null;
	content: string;
	content_html?: string;
	protection_level: ProtectionLevel;
	categories: CategoryView[];
	tags: string[];
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

/**
 * Body for `POST /api/v1/pages` and `POST /api/v1/wiki/{namespace}`.
 *
 * `namespace_slug` is optional on the namespace-aware route — the
 * namespace comes from the URL there. It remains required-by-convention
 * for the legacy `/api/v1/pages` call so existing clients keep working
 * without code changes.
 */
export interface CreatePageRequest {
	namespace_slug?: string;
	slug: string;
	title: string;
	content: string;
	categories?: string[];
	tags?: string[];
}

/** Body for `PUT /api/v1/pages/{slug}`. */
export interface UpdatePageRequest {
	title?: string;
	content: string;
	edit_summary?: string;
	categories?: string[];
	tags?: string[];
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

/**
 * Fetch a page by slug.
 *
 * Resolves through the namespace-aware route `/api/v1/wiki/{namespace}/{slug}`,
 * which falls back to `Main` when no namespace is provided.
 */
export async function fetchPage(slug: string, namespace?: string): Promise<PageView> {
	return jsonRequest<PageView>(wikiPath(namespace, slug), { method: "GET" });
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

/**
 * Create a page. The `namespace_slug` on `body` selects the target
 * namespace; the helper routes through `/api/v1/wiki/{namespace}` so the
 * back-end can pick up the namespace from the URL.
 */
export async function createPage(body: CreatePageRequest): Promise<PageView> {
	const namespace = resolveNamespace(body.namespace_slug);
	return jsonRequest<PageView>(`/api/v1/wiki/${encodeURIComponent(namespace)}`, {
		method: "POST",
		headers: authHeaders(),
		body: JSON.stringify(body),
	});
}

/**
 * Commit a new revision of a page. Routes through the namespace-aware
 * surface so URLs stay consistent with the read path.
 */
export async function updatePage(
	slug: string,
	body: UpdatePageRequest,
	namespace?: string,
): Promise<PageView> {
	return jsonRequest<PageView>(wikiPath(namespace, slug), {
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
export async function protectPage(
	slug: string,
	body: ProtectPageRequest,
	namespace?: string,
): Promise<PageView> {
	return jsonRequest<PageView>(`${wikiPath(namespace, slug)}/protect`, {
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

/** Mirrors `NamespaceView` from `crates/api/src/namespaces/dto.rs`. */
export interface NamespaceView {
	id: string;
	slug: string;
	display_name: string;
}

/** Response from `GET /api/v1/namespaces`. */
export interface NamespaceListResponse {
	items: NamespaceView[];
}

/**
 * Fetch every namespace defined on this wiki. Used by the SPA to render
 * namespace prefixes on search results and to drive the (future)
 * namespace switcher.
 */
export async function listNamespaces(): Promise<NamespaceListResponse> {
	return jsonRequest<NamespaceListResponse>("/api/v1/namespaces", { method: "GET" });
/** Response from `GET /api/v1/categories`. */
export interface CategoryListResponse {
	items: CategoryView[];
}

/** A member entry inside `GET /api/v1/categories/{slug}` / `GET /api/v1/tags/{tag}`. */
export interface CategoryMemberView {
	page_id: string;
	namespace_slug: string;
	slug: string;
	title: string;
}

/** Response from `GET /api/v1/categories/{slug}`. */
export interface CategoryDetailResponse {
	category: CategoryView;
	items: CategoryMemberView[];
	next_cursor: string | null;
}

/** Response from `GET /api/v1/tags/{tag}`. */
export interface TagDetailResponse {
	tag: string;
	items: CategoryMemberView[];
	next_cursor: string | null;
}

/** Response from `GET /api/v1/tags?prefix=...`. */
export interface TagListResponse {
	items: string[];
}

/** List every defined category. */
export async function listCategories(): Promise<CategoryListResponse> {
	return jsonRequest<CategoryListResponse>("/api/v1/categories", { method: "GET" });
}

/** Fetch a category + its member pages. */
export async function fetchCategory(
	slug: string,
	options?: { cursor?: string | null; limit?: number },
): Promise<CategoryDetailResponse> {
	const params = new URLSearchParams();
	if (options?.cursor) {
		params.set("cursor", options.cursor);
	}
	if (options?.limit !== undefined) {
		params.set("limit", String(options.limit));
	}
	const query = params.toString();
	const url =
		query.length > 0
			? `/api/v1/categories/${encodeURIComponent(slug)}?${query}`
			: `/api/v1/categories/${encodeURIComponent(slug)}`;
	return jsonRequest<CategoryDetailResponse>(url, { method: "GET" });
}

/** Fetch the pages carrying a specific tag. */
export async function fetchTag(
	tag: string,
	options?: { cursor?: string | null; limit?: number },
): Promise<TagDetailResponse> {
	const params = new URLSearchParams();
	if (options?.cursor) {
		params.set("cursor", options.cursor);
	}
	if (options?.limit !== undefined) {
		params.set("limit", String(options.limit));
	}
	const query = params.toString();
	const url =
		query.length > 0
			? `/api/v1/tags/${encodeURIComponent(tag)}?${query}`
			: `/api/v1/tags/${encodeURIComponent(tag)}`;
	return jsonRequest<TagDetailResponse>(url, { method: "GET" });
}

/** Autocomplete tags by prefix. Empty prefix lists every tag, clamped by `limit`. */
export async function autocompleteTags(prefix: string, limit?: number): Promise<TagListResponse> {
	const params = new URLSearchParams();
	if (prefix.length > 0) {
		params.set("prefix", prefix);
	}
	if (limit !== undefined) {
		params.set("limit", String(limit));
	}
	const query = params.toString();
	const url = query.length > 0 ? `/api/v1/tags?${query}` : "/api/v1/tags";
	return jsonRequest<TagListResponse>(url, { method: "GET" });
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
