import { useEffect, useState } from "react";

import { getNextRetryAt } from "../lib/commands";
import type { DownloadSummary } from "../lib/ipc";
import { retryAtOf } from "../lib/messages";

/** Wait after a fetched retry time has passed before asking again, so a new wait is noticed. */
const REFRESH_AFTER_PAST_MS = 2000;

/**
 * When a `retry_waiting` download will retry. A time carried by the list wins; otherwise
 * `next_retry_at_ms` is read from the download's detail when the row enters the state, and
 * read again once that time has passed (the engine may have started a new wait since).
 */
export function useRetryAt(item: DownloadSummary): number | undefined {
  const inline = retryAtOf(item);
  const waiting = item.state === "retry_waiting";
  const [fetched, setFetched] = useState<{ id: string; at: number | undefined }>();
  const [refresh, setRefresh] = useState(0);
  const [wasWaiting, setWasWaiting] = useState(waiting);
  // Leaving retry_waiting forgets the fetched time, so a later wait never shows a stale one.
  // Adjusted during render (React's pattern for state that follows a prop), not in an effect.
  if (wasWaiting !== waiting) {
    setWasWaiting(waiting);
    if (!waiting) setFetched(undefined);
  }
  const fetchedAt = fetched?.id === item.id ? fetched.at : undefined;

  useEffect(() => {
    if (!waiting || inline !== undefined) return;
    let cancelled = false;
    getNextRetryAt(item.id).then(
      (at) => {
        if (!cancelled) setFetched({ id: item.id, at });
      },
      () => undefined,
    );
    return () => {
      cancelled = true;
    };
  }, [waiting, inline, item.id, refresh]);

  useEffect(() => {
    if (!waiting || inline !== undefined || fetchedAt === undefined) return;
    const delay = Math.max(fetchedAt - Date.now(), 0) + REFRESH_AFTER_PAST_MS;
    const timer = setTimeout(() => setRefresh((count) => count + 1), delay);
    return () => clearTimeout(timer);
  }, [waiting, inline, fetchedAt]);

  if (!waiting) return undefined;
  return inline ?? fetchedAt;
}
