import { useCallback, useEffect, useRef } from "react";

import mark from "./assets/forgedm-mark.svg";
import { AddDownload } from "./components/AddDownload";
import { DownloadList } from "./components/DownloadList";
import { EmptyState } from "./components/EmptyState";
import { EngineBanner } from "./components/EngineBanner";
import { StatusStrip } from "./components/StatusStrip";
import { StopEngine } from "./components/StopEngine";
import { Toasts } from "./components/Toasts";
import { messageOf, useEngine, type EngineControl } from "./hooks/useEngine";
import { useRowRequests } from "./hooks/useRowRequests";
import { useSnapshotPolling, type PollState } from "./hooks/useSnapshotPolling";
import { useToasts } from "./hooks/useToasts";

function pillFor(state: PollState): { text: string; tone: string } {
  switch (state.phase) {
    case "ready":
      return state.stale === undefined ? { text: "Engine running", tone: "ok" } : { text: "Engine not responding", tone: "bad" };
    case "unavailable":
      return { text: "Engine offline", tone: "off" };
    case "error":
      return { text: "Engine error", tone: "bad" };
    default:
      return { text: "Connecting", tone: "wait" };
  }
}

interface BodyProps {
  state: PollState;
  engine: EngineControl;
  refresh: () => void;
  onAdded: (fileName: string) => void;
  onRowError: (message: string) => void;
}

function Body({ state, engine, refresh, onAdded, onRowError }: BodyProps) {
  const onRequest = useRowRequests(onRowError, refresh);
  switch (state.phase) {
    case "loading":
      return <p className="muted">Connecting to the background engine…</p>;
    case "unavailable":
      return (
        <EngineBanner starting={engine.starting} error={engine.startError} onStart={() => void engine.start()} onCheck={refresh} />
      );
    case "error":
      return (
        <EmptyState title={state.title} action={{ label: "Check again", onClick: refresh }} tone="warning">
          {state.detail}
        </EmptyState>
      );
    case "ready": {
      const { agent_status: status, downloads } = state.snapshot;
      return (
        <>
          {state.stale !== undefined && (
            <EmptyState title={state.stale.title} action={{ label: "Check again", onClick: refresh }} tone="warning">
              {state.stale.detail} Showing the last known downloads.
            </EmptyState>
          )}
          <StatusStrip status={status} />
          <AddDownload onAdded={onAdded} />
          {downloads.length === 0 ? (
            <EmptyState title="No downloads yet">Downloads you add will appear here with live progress.</EmptyState>
          ) : (
            <DownloadList items={downloads} onRequest={onRequest} />
          )}
        </>
      );
    }
  }
}

export default function App() {
  const { state, refresh } = useSnapshotPolling();
  const { toasts, push, dismiss } = useToasts();
  const engine = useEngine(refresh);
  const autoStarted = useRef(false);
  const { start } = engine;

  // Auto-start once at launch: the first time the engine is found not running.
  useEffect(() => {
    if (state.phase === "unavailable" && !autoStarted.current) {
      autoStarted.current = true;
      void start();
    }
  }, [state.phase, start]);

  const onAdded = useCallback(
    (fileName: string) => {
      push("info", `Added ${fileName}`);
      refresh();
    },
    [push, refresh],
  );
  const onRowError = useCallback((message: string) => push("error", message), [push]);
  const { stop } = engine;
  const onStop = useCallback(() => {
    stop().catch((err: unknown) => push("error", messageOf(err)));
  }, [stop, push]);

  const pill = pillFor(state);
  return (
    <div className="shell">
      <header className="topbar">
        <img src={mark} alt="" width={28} height={28} />
        <h1>ForgeDM</h1>
        <span className={`pill pill-${pill.tone}`}>
          <span className="pill-dot" aria-hidden="true" />
          {pill.text}
        </span>
        {state.phase === "ready" && <StopEngine onStop={onStop} />}
      </header>
      <main className="content">
        <Body state={state} engine={engine} refresh={refresh} onAdded={onAdded} onRowError={onRowError} />
      </main>
      <Toasts toasts={toasts} onDismiss={dismiss} />
    </div>
  );
}
