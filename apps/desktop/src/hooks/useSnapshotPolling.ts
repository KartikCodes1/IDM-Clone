import { useCallback, useEffect, useRef, useState } from "react";

import { getSnapshot } from "../lib/commands";
import { CommandFailure, PayloadError, type EngineSnapshot } from "../lib/ipc";

export type PollState =
  | { phase: "loading" }
  | { phase: "ready"; snapshot: EngineSnapshot; stale?: { title: string; detail: string } }
  | { phase: "unavailable" }
  | { phase: "error"; title: string; detail: string };

/** One `GetSnapshot` per second while the window is visible (event subscription is Phase 2). */
export const POLL_INTERVAL_MS = 1000;

function toErrorState(err: unknown): PollState {
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

/**
 * A transient failure must not unmount the working UI (typed form input, open confirmations).
 * Once a snapshot has been shown, a failed poll keeps it and records a stale notice. A stopped
 * engine (`unavailable`) still replaces the view, because nothing can be done until it starts.
 */
function keepLastGood(prev: PollState, failure: PollState): PollState {
  if (prev.phase === "ready" && failure.phase === "error") {
    return { phase: "ready", snapshot: prev.snapshot, stale: { title: failure.title, detail: failure.detail } };
  }
  return failure;
}

function windowIsVisible(): boolean {
  return document.visibilityState !== "hidden";
}

/**
 * Polls the engine for a snapshot. Polling runs only while the document is visible, is
 * restarted (with an immediate poll) when it becomes visible again, never overlaps
 * requests, and stops for good on unmount.
 */
export function useSnapshotPolling(): { state: PollState; refresh: () => void } {
  const [state, setState] = useState<PollState>({ phase: "loading" });
  const inFlight = useRef(false);
  const pending = useRef(false);
  const mounted = useRef(true);

  const refresh = useCallback(function run() {
    if (inFlight.current) {
      // Run one more poll when the current one settles, so a refresh after an action is not lost.
      pending.current = true;
      return;
    }
    inFlight.current = true;
    void getSnapshot()
      .then((snapshot) => {
        if (mounted.current) setState({ phase: "ready", snapshot });
      })
      .catch((err: unknown) => {
        if (mounted.current) setState((prev) => keepLastGood(prev, toErrorState(err)));
      })
      .finally(() => {
        inFlight.current = false;
        if (pending.current && mounted.current) {
          pending.current = false;
          run();
        }
      });
  }, []);

  useEffect(() => {
    mounted.current = true;
    let timer: ReturnType<typeof setInterval> | undefined;
    const stop = () => {
      if (timer !== undefined) clearInterval(timer);
      timer = undefined;
    };
    const start = () => {
      stop();
      refresh();
      timer = setInterval(refresh, POLL_INTERVAL_MS);
    };
    const onVisibility = () => (windowIsVisible() ? start() : stop());
    if (windowIsVisible()) start();
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      mounted.current = false;
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [refresh]);

  return { state, refresh };
}
