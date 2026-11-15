import { DownloadList } from "./components/DownloadList";
import { EmptyState } from "./components/EmptyState";
import { StatusStrip } from "./components/StatusStrip";
import { useAgentSnapshot, type Snapshot } from "./hooks/useAgentSnapshot";
import mark from "./assets/forgedm-mark.svg";

function pillFor(snapshot: Snapshot): { text: string; tone: string } {
  switch (snapshot.phase) {
    case "ready":
      return { text: "Engine running", tone: "ok" };
    case "unavailable":
      return { text: "Engine offline", tone: "off" };
    case "error":
      return { text: "Engine error", tone: "bad" };
    default:
      return { text: "Connecting", tone: "wait" };
  }
}

function Body({ snapshot, refresh }: { snapshot: Snapshot; refresh: () => void }) {
  const check = { label: "Check again", onClick: refresh };
  switch (snapshot.phase) {
    case "loading":
      return <p className="muted">Connecting to the background engine…</p>;
    case "unavailable":
      return (
        <EmptyState title="Background engine not running" action={check} tone="warning">
          ForgeDM downloads run in a separate background engine. Start it, then check again.
        </EmptyState>
      );
    case "error":
      return (
        <EmptyState title={snapshot.title} action={check} tone="warning">
          {snapshot.detail}
        </EmptyState>
      );
    case "ready":
      return (
        <>
          <StatusStrip status={snapshot.status} />
          {snapshot.downloads.items.length === 0 ? (
            <EmptyState title="No downloads yet">
              Downloads you add will appear here with live progress.
            </EmptyState>
          ) : (
            <DownloadList items={snapshot.downloads.items} />
          )}
        </>
      );
  }
}

export default function App() {
  const { snapshot, refresh } = useAgentSnapshot();
  const pill = pillFor(snapshot);
  return (
    <div className="shell">
      <header className="topbar">
        <img src={mark} alt="" width={28} height={28} />
        <h1>ForgeDM</h1>
        <span className={`pill pill-${pill.tone}`}>
          <span className="pill-dot" aria-hidden="true" />
          {pill.text}
        </span>
      </header>
      <main className="content">
        <Body snapshot={snapshot} refresh={refresh} />
      </main>
    </div>
  );
}
