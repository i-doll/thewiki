//! Reviewer landing page for the edit approval queue (`/admin/approval-queue`) — #40.
//!
//! Lists pending revisions, filterable by status. Clicking an entry opens
//! the detail panel: side-by-side diff (proposed body vs. current head)
//! plus approve / reject controls. Reject opens a dialog asking for the
//! reason which is then echoed back to the original (authenticated)
//! author through the in-app inbox.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { useState } from "react";
import toast from "react-hot-toast";
import ReactDiffViewer, { DiffMethod } from "react-diff-viewer-continued";
import {
	ApiError,
	approvePendingRevision,
	fetchPendingRevision,
	listPendingRevisions,
	type PendingRevisionDetailResponse,
	type PendingRevisionListResponse,
	type PendingRevisionStatus,
	type PendingRevisionView,
	rejectPendingRevision,
} from "../lib/api";

export const Route = createFileRoute("/admin/approval-queue")({
	component: ApprovalQueueComponent,
	validateSearch: (search: Record<string, unknown>) => {
		const status = typeof search.status === "string" ? search.status : "pending";
		const selected = typeof search.selected === "string" ? search.selected : "";
		return { status: status as PendingRevisionStatus, selected };
	},
});

const STATUSES: { value: PendingRevisionStatus; label: string }[] = [
	{ value: "pending", label: "Pending" },
	{ value: "approved", label: "Approved" },
	{ value: "rejected", label: "Rejected" },
];

function ApprovalQueueComponent() {
	const { status, selected } = Route.useSearch();
	const navigate = Route.useNavigate();

	const list = useQuery<PendingRevisionListResponse, ApiError>({
		queryKey: ["pending-revisions", status],
		queryFn: () => listPendingRevisions({ status, limit: 50 }),
		retry: false,
	});

	const detail = useQuery<PendingRevisionDetailResponse, ApiError>({
		queryKey: ["pending-revision", selected],
		queryFn: () => fetchPendingRevision(selected),
		enabled: selected.length > 0,
		retry: false,
	});

	if (list.isError && list.error.status === 403) {
		return (
			<main className="mx-auto max-w-3xl px-6 py-12">
				<h1 className="text-xl font-semibold">Approval queue</h1>
				<p className="mt-2 text-sm text-neutral-600">
					You don't have permission to review queued edits. Ask an
					administrator for the <code>REVIEW_EDITS</code> permission.
				</p>
			</main>
		);
	}
	if (list.isError && list.error.status === 401) {
		return (
			<main className="mx-auto max-w-3xl px-6 py-12">
				<h1 className="text-xl font-semibold">Approval queue</h1>
				<p className="mt-2 text-sm text-neutral-600">
					Please <Link to="/login" className="text-blue-700 underline">log in</Link>{" "}
					to review queued edits.
				</p>
			</main>
		);
	}

	return (
		<main className="mx-auto max-w-6xl px-6 py-6">
			<header className="mb-6 flex items-center justify-between">
				<h1 className="text-xl font-semibold">Approval queue</h1>
				<div className="flex items-center gap-2 text-sm">
					{STATUSES.map((s) => (
						<button
							type="button"
							key={s.value}
							onClick={() =>
								navigate({
									search: { status: s.value, selected: "" },
								})
							}
							className={
								status === s.value
									? "rounded border border-neutral-800 bg-neutral-900 px-3 py-1 text-white"
									: "rounded border border-neutral-200 px-3 py-1 hover:bg-neutral-100"
							}
						>
							{s.label}
						</button>
					))}
				</div>
			</header>

			<div className="grid gap-6 md:grid-cols-[20rem_1fr]">
				<aside className="rounded border border-neutral-200 bg-white">
					<div className="border-b border-neutral-100 px-3 py-2 text-xs font-semibold uppercase tracking-wide text-neutral-500">
						{list.data?.total ?? 0} {status}
					</div>
					{list.isPending ? (
						<div className="px-3 py-4 text-sm text-neutral-500">Loading…</div>
					) : list.data && list.data.items.length === 0 ? (
						<div className="px-3 py-4 text-sm text-neutral-500">
							Nothing in this view.
						</div>
					) : (
						<ul className="max-h-[70vh] overflow-auto">
							{list.data?.items.map((item) => (
								<ListEntry
									key={item.id}
									item={item}
									selected={item.id === selected}
									onClick={() =>
										navigate({
											search: { status, selected: item.id },
										})
									}
								/>
							))}
						</ul>
					)}
				</aside>

				<section>
					{selected.length === 0 ? (
						<div className="rounded border border-dashed border-neutral-300 bg-white px-6 py-12 text-center text-sm text-neutral-500">
							Select a queued edit to review it.
						</div>
					) : detail.isPending ? (
						<div className="rounded border border-neutral-200 bg-white px-6 py-12 text-sm text-neutral-500">
							Loading…
						</div>
					) : detail.data ? (
						<DetailPanel detail={detail.data} />
					) : (
						<div className="rounded border border-red-200 bg-red-50 px-6 py-4 text-sm text-red-700">
							Couldn't load that pending revision.
						</div>
					)}
				</section>
			</div>
		</main>
	);
}

function ListEntry({
	item,
	selected,
	onClick,
}: {
	item: PendingRevisionView;
	selected: boolean;
	onClick: () => void;
}) {
	return (
		<li>
			<button
				type="button"
				onClick={onClick}
				className={
					selected
						? "block w-full border-l-2 border-blue-500 bg-blue-50 px-3 py-2 text-left"
						: "block w-full px-3 py-2 text-left hover:bg-neutral-50"
				}
			>
				<div className="text-sm font-medium text-neutral-900">
					{item.page_title}
				</div>
				<div className="text-xs text-neutral-600">
					{item.namespace_slug}/{item.page_slug}
				</div>
				<div className="mt-1 text-xs text-neutral-500">
					{item.author_label} · {new Date(item.created_at).toLocaleString()}
				</div>
			</button>
		</li>
	);
}

function DetailPanel({ detail }: { detail: PendingRevisionDetailResponse }) {
	const queryClient = useQueryClient();
	const [showReject, setShowReject] = useState(false);
	const [reason, setReason] = useState("");

	const approve = useMutation({
		mutationFn: () => approvePendingRevision(detail.id),
		onSuccess: () => {
			toast.success("Edit approved.");
			queryClient.invalidateQueries({ queryKey: ["pending-revisions"] });
			queryClient.invalidateQueries({ queryKey: ["pending-revision", detail.id] });
			// Also refresh the relevant page view in case the reviewer
			// navigates over to it next.
			queryClient.invalidateQueries({
				queryKey: ["page", detail.namespace_slug, detail.page_slug],
			});
		},
		onError: (err: ApiError) => {
			toast.error(err.message);
		},
	});

	const reject = useMutation({
		mutationFn: () => rejectPendingRevision(detail.id, reason),
		onSuccess: () => {
			toast.success("Edit rejected.");
			setShowReject(false);
			setReason("");
			queryClient.invalidateQueries({ queryKey: ["pending-revisions"] });
			queryClient.invalidateQueries({ queryKey: ["pending-revision", detail.id] });
		},
		onError: (err: ApiError) => {
			toast.error(err.message);
		},
	});

	const isPending = detail.status === "pending";

	return (
		<article className="rounded border border-neutral-200 bg-white">
			<header className="border-b border-neutral-100 px-6 py-4">
				<div className="flex items-start justify-between gap-3">
					<div>
						<h2 className="text-lg font-semibold text-neutral-900">
							{detail.page_title}
						</h2>
						<div className="text-xs text-neutral-600">
							<Link
								to="/wiki/$namespace/$slug"
								params={{
									namespace: detail.namespace_slug,
									slug: detail.page_slug,
								}}
								className="hover:text-neutral-900"
							>
								{detail.namespace_slug}/{detail.page_slug}
							</Link>
						</div>
					</div>
					<StatusBadge status={detail.status} />
				</div>
				<dl className="mt-3 grid grid-cols-2 gap-1 text-xs text-neutral-600">
					<dt>Author</dt>
					<dd>{detail.author_label}</dd>
					<dt>Queued</dt>
					<dd>{new Date(detail.created_at).toLocaleString()}</dd>
					{detail.comment ? (
						<>
							<dt>Comment</dt>
							<dd className="whitespace-pre-wrap">{detail.comment}</dd>
						</>
					) : null}
					{detail.rejection_reason ? (
						<>
							<dt>Rejection reason</dt>
							<dd className="whitespace-pre-wrap text-red-700">
								{detail.rejection_reason}
							</dd>
						</>
					) : null}
				</dl>
				{isPending && (
					<div className="mt-4 flex items-center gap-2">
						<button
							type="button"
							onClick={() => approve.mutate()}
							disabled={approve.isPending}
							className="rounded bg-green-600 px-3 py-1 text-sm font-medium text-white hover:bg-green-700 disabled:opacity-50"
						>
							{approve.isPending ? "Approving…" : "Approve"}
						</button>
						<button
							type="button"
							onClick={() => setShowReject(true)}
							className="rounded border border-red-600 px-3 py-1 text-sm font-medium text-red-700 hover:bg-red-50"
						>
							Reject
						</button>
					</div>
				)}
			</header>
			<div className="px-6 py-4">
				<h3 className="mb-2 text-xs font-semibold uppercase tracking-wide text-neutral-500">
					Proposed diff
				</h3>
				<ReactDiffViewer
					oldValue={detail.parent_body ?? ""}
					newValue={detail.body}
					compareMethod={DiffMethod.LINES}
					splitView
					useDarkTheme={false}
					hideLineNumbers={false}
					styles={{
						contentText: { fontSize: "12px", fontFamily: "ui-monospace, monospace" },
					}}
				/>
			</div>
			{showReject && (
				<RejectDialog
					reason={reason}
					setReason={setReason}
					onClose={() => setShowReject(false)}
					onConfirm={() => reject.mutate()}
					submitting={reject.isPending}
				/>
			)}
		</article>
	);
}

function StatusBadge({ status }: { status: PendingRevisionStatus }) {
	const colours: Record<PendingRevisionStatus, string> = {
		pending: "bg-yellow-100 text-yellow-800",
		approved: "bg-green-100 text-green-800",
		rejected: "bg-red-100 text-red-800",
	};
	return (
		<span
			className={`rounded px-2 py-1 text-xs font-semibold uppercase tracking-wide ${colours[status]}`}
		>
			{status}
		</span>
	);
}

function RejectDialog({
	reason,
	setReason,
	onClose,
	onConfirm,
	submitting,
}: {
	reason: string;
	setReason: (s: string) => void;
	onClose: () => void;
	onConfirm: () => void;
	submitting: boolean;
}) {
	return (
		<div className="fixed inset-0 z-30 flex items-center justify-center bg-neutral-900/40 px-6 py-12">
			<div className="w-full max-w-md rounded border border-neutral-200 bg-white p-6 shadow-lg">
				<h3 className="text-base font-semibold text-neutral-900">Reject this edit</h3>
				<p className="mt-1 text-sm text-neutral-600">
					The reason is stored in the audit log and sent to the original author
					through their inbox.
				</p>
				<textarea
					className="mt-3 h-32 w-full rounded border border-neutral-300 px-3 py-2 text-sm"
					value={reason}
					placeholder="Explain why this edit is being rejected."
					onChange={(e) => setReason(e.target.value)}
				/>
				<div className="mt-4 flex items-center justify-end gap-2">
					<button
						type="button"
						onClick={onClose}
						className="rounded border border-neutral-300 px-3 py-1 text-sm hover:bg-neutral-50"
					>
						Cancel
					</button>
					<button
						type="button"
						onClick={onConfirm}
						disabled={submitting || reason.trim().length === 0}
						className="rounded bg-red-600 px-3 py-1 text-sm font-medium text-white hover:bg-red-700 disabled:opacity-50"
					>
						{submitting ? "Rejecting…" : "Reject"}
					</button>
				</div>
			</div>
		</div>
	);
}
