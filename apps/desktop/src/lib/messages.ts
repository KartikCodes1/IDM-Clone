// Friendly, fixed text for the states that need an explanation (PLAN A.6/A.7). Nothing
// from the engine's own message strings is shown.

import { failureText } from "./actions";
import type { DownloadFailure } from "./ipc";

const TIME_FORMAT = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
const DAY_TIME_FORMAT = new Intl.DateTimeFormat(undefined, {
  weekday: "short",
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

function sameLocalDay(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

/** Local time of a retry; the date is added when it is not today. */
export function formatRetryTime(atMs: number, nowMs: number = Date.now()): string {
  const at = new Date(atMs);
  return sameLocalDay(at, new Date(nowMs)) ? TIME_FORMAT.format(at) : DAY_TIME_FORMAT.format(at);
}

/** The retry time a rate-limit error carries, when there is one. */
export function retryAtOf(item: { last_error?: DownloadFailure }): number | undefined {
  const at = item.last_error?.details?.retry_at_ms;
  return at !== undefined && Number.isFinite(new Date(at).getTime()) ? at : undefined;
}

const PAUSED_REASON_TEXT: Readonly<Record<string, string>> = {
  disk_full: "Paused: the disk is full. Free some space, then resume.",
  destination_unavailable: "Paused: the destination folder or drive is unavailable. Reconnect it, then resume.",
};

interface MessageSource {
  state: string;
  state_reason?: string;
  last_error?: DownloadFailure | { code: string; message: string; retryable: boolean };
}

function retryMessage(retryAtMs: number | undefined, nowMs: number): string {
  if (retryAtMs === undefined || !Number.isFinite(new Date(retryAtMs).getTime())) return "Retrying soon";
  return retryAtMs <= nowMs ? "Retrying now" : `Retrying at ${formatRetryTime(retryAtMs, nowMs)}`;
}

/** A sentence for the row, or undefined when the state needs no explanation. */
export function stateMessage(item: MessageSource, retryAtMs?: number, nowMs: number = Date.now()): string | undefined {
  switch (item.state) {
    case "retry_waiting":
      return retryMessage(retryAtMs, nowMs);
    case "needs_url_refresh":
      return "Link expired: add the download again with a fresh URL";
    case "needs_credentials":
      return "This server needs a login (not supported yet)";
    case "interrupted":
      return "Will resume automatically";
    case "paused":
      return item.state_reason !== undefined && Object.hasOwn(PAUSED_REASON_TEXT, item.state_reason)
        ? PAUSED_REASON_TEXT[item.state_reason]
        : undefined;
    case "failed":
      return failureText(item.last_error);
    default:
      return undefined;
  }
}
