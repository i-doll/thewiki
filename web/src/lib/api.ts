//! Typed wrappers around the page CRUD endpoints in `crates/api/src/pages`.
//!
//! Mutating requests inject the placeholder `X-User-Id` header (see
//! `lib/auth.ts`). Read endpoints don't carry the header — the API only
//! requires auth on writes today.

import { getCurrentUserId } from "./auth";

/** Mirrors `PageView` from `crates/api/src/pages/dto.rs`. */
export interface PageView {
	id: string;
	namespace_id: string;
	namespace_slug: string;
	slug: string;
	title: string;
	current_revision_id: string | null;
	content: string;
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
