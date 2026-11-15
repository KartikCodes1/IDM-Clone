import { useCallback, useEffect, useRef, useState } from "react";

import {
  CommandFailure,
  PayloadError,
  getAgentStatus,
  listDownloads,
  type AgentStatusInfo,
  type DownloadList,
} from "../lib/ipc";

export type Snapshot =
  | { phase: "loading" }
  | { phase: "ready"; status: AgentStatusInfo; downloads: DownloadList }
  | { phase: "unavailable" }
  | { phase: "error"; title: string; detail: string };

const POLL_INTERVAL_MS = 2000;

function toErrorSnapshot(err: unknown): Snapshot {
  if (err instanceof CommandFailure) {
    if (err.error.kind === "agent_unavailable") return { phase: "unavailable" };
    return { phase: "error", title: "Engine request failed", detail: err.error.message };
  }
  if (err instanceof PayloadError) {
    return {
      phase: "error",
      title: "Unexpected answer from the engine",
      detail: "The engine replied in a format this app does not understand.",
    };
  }
  return { phase: "error", title: "Engine request failed", detail: "An unexpected error occurred." };
}

/** Polls the agent (Phase 0: per-call connections; subscriptions arrive in Phase 2). */
export function useAgentSnapshot(): { snapshot: Snapshot; refresh: () => void } {
  const [snapshot, setSnapshot] = useState<Snapshot>({ phase: "loading" });
  const inFlight = useRef(false);
  const active = useRef(true);

  const refresh = useCallback(() => {
    if (inFlight.current) return;
    inFlight.current = true;
    void Promise.all([getAgentStatus(), listDownloads()])
      .then(([status, downloads]) => {
        if (active.current) setSnapshot({ phase: "ready", status, downloads });
      })
      .catch((err: unknown) => {
        if (active.current) setSnapshot(toErrorSnapshot(err));
      })
      .finally(() => {
        inFlight.current = false;
      });
  }, []);

  useEffect(() => {
    active.current = true;
    refresh();
    const timer = setInterval(refresh, POLL_INTERVAL_MS);
    return () => {
      active.current = false;
      clearInterval(timer);
    };
  }, [refresh]);

  return { snapshot, refresh };
}
