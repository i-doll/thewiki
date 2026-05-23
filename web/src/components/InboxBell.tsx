//! Header inbox bell — surfaces unread approval decisions (#40).
//!
//! Polls the inbox every 60 s for authenticated users; anonymous callers
//! see nothing (the endpoint returns 401 → null and we render
//! nothing). Clicking the bell opens a dropdown with the unread entries;
//! clicking an entry marks it read, dismisses the dropdown, and (for
//! pending-revision decisions) navigates to the relevant page.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import {
	listNotifications,
	markNotificationRead,
	type NotificationListResponse,
	type NotificationView,
} from "../lib/api";

interface PendingRevisionPayload {
	pending_revision_id?: string;
	page_id?: string;
	namespace_slug?: string;
	page_slug?: string;
	page_title?: string;
	reason?: string;
}

function payloadOf(notif: NotificationView): PendingRevisionPayload {
	if (notif.payload && typeof notif.payload === "object") {
		return notif.payload as PendingRevisionPayload;
	}
	return {};
}

function notificationLabel(notif: NotificationView): string {
	const payload = payloadOf(notif);
	const title = payload.page_title ?? payload.page_slug ?? "your edit";
	switch (notif.kind) {
		case "pending_revision_approved":
			return `Your edit to ${title} was approved`;
		case "pending_revision_rejected":
			return `Your edit to ${title} was rejected${
				payload.reason ? `: ${payload.reason}` : ""
			}`;
		default:
			return notif.kind;
	}
}

interface PageRoute {
	namespace: string;
	slug: string;
}

function notificationLink(notif: NotificationView): PageRoute | null {
	const payload = payloadOf(notif);
	if (payload.namespace_slug && payload.page_slug) {
		return { namespace: payload.namespace_slug, slug: payload.page_slug };
	}
	return null;
}

export function InboxBell() {
	const [open, setOpen] = useState(false);
	const queryClient = useQueryClient();

	const query = useQuery<NotificationListResponse | null>({
		queryKey: ["notifications", "list"],
		queryFn: () => listNotifications({ limit: 20 }),
		refetchInterval: 60_000,
		retry: false,
	});

	const markRead = useMutation({
		mutationFn: markNotificationRead,
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["notifications", "list"] });
		},
	});

	// Anonymous callers (or rate-limited / network errors) → render nothing.
	if (!query.data) {
		return null;
	}

	const unread = query.data.unread;
	const items = query.data.items;

	return (
		<div className="relative">
			<button
				type="button"
				className="relative flex h-8 w-8 items-center justify-center rounded text-neutral-600 hover:bg-neutral-100 hover:text-neutral-900"
				onClick={() => setOpen((o) => !o)}
				aria-label={`Notifications (${unread} unread)`}
			>
				<BellIcon />
				{unread > 0 && (
					<span className="absolute -top-1 -right-1 flex h-4 min-w-4 items-center justify-center rounded-full bg-red-500 px-1 text-[10px] font-semibold text-white">
						{unread > 9 ? "9+" : unread}
					</span>
				)}
			</button>
			{open && (
				<div className="absolute right-0 z-20 mt-2 w-80 rounded border border-neutral-200 bg-white shadow-lg">
					<div className="flex items-center justify-between border-b border-neutral-100 px-3 py-2 text-xs font-semibold uppercase tracking-wide text-neutral-500">
						<span>Inbox</span>
						<span>{unread} unread</span>
					</div>
					{items.length === 0 ? (
						<div className="px-3 py-6 text-center text-sm text-neutral-500">
							No notifications.
						</div>
					) : (
						<ul className="max-h-80 overflow-auto">
							{items.map((item) => {
								const route = notificationLink(item);
								const onClick = () => {
									if (item.read_at === null) {
										markRead.mutate(item.id);
									}
									setOpen(false);
								};
								const body = (
									<>
										<div className="text-sm text-neutral-900">
											{notificationLabel(item)}
										</div>
										<div className="text-xs text-neutral-500">
											{new Date(item.created_at).toLocaleString()}
										</div>
									</>
								);
								return (
									<li
										key={item.id}
										className={
											item.read_at === null
												? "bg-blue-50 hover:bg-blue-100"
												: "hover:bg-neutral-50"
										}
									>
										{route ? (
											<Link
												to="/wiki/$namespace/$slug"
												params={route}
												onClick={onClick}
												className="block px-3 py-2"
											>
												{body}
											</Link>
										) : (
											<button
												type="button"
												onClick={onClick}
												className="block w-full px-3 py-2 text-left"
											>
												{body}
											</button>
										)}
									</li>
								);
							})}
						</ul>
					)}
				</div>
			)}
		</div>
	);
}

function BellIcon() {
	return (
		<svg
			xmlns="http://www.w3.org/2000/svg"
			width="16"
			height="16"
			viewBox="0 0 24 24"
			fill="none"
			stroke="currentColor"
			strokeWidth="2"
			strokeLinecap="round"
			strokeLinejoin="round"
			aria-hidden="true"
		>
			<path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" />
			<path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" />
		</svg>
	);
}
