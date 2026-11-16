import { useState } from "react";

import { messageOf } from "../hooks/useEngine";
import { pauseAll, resumeAll } from "../lib/commands";
import { LimitsPanel } from "./LimitsPanel";

interface Props {
  notify: (tone: "info" | "error", message: string) => void;
  refresh: () => void;
}

function plural(count: number): string {
  return count === 1 ? "1 download" : `${count} downloads`;
}

/** Batch controls and the limits panel toggle. */
export function Toolbar({ notify, refresh }: Props) {
  const [busy, setBusy] = useState(false);
  const [limitsOpen, setLimitsOpen] = useState(false);

  async function run(task: () => Promise<void>) {
    if (busy) return;
    setBusy(true);
    try {
      await task();
    } catch (err) {
      notify("error", messageOf(err));
    } finally {
      setBusy(false);
      refresh();
    }
  }

  const onPauseAll = () =>
    run(async () => {
      const result = await pauseAll(false);
      notify("info", `Paused ${plural(result.changed)}`);
      if (result.skipped > 0) {
        const who = result.skipped === 1 ? "it was" : "they were";
        notify(
          "info",
          `${plural(result.skipped)} cannot resume, so ${who} left running. Pause them one by one to discard their progress.`,
        );
      }
    });

  const onResumeAll = () =>
    run(async () => {
      const result = await resumeAll();
      notify("info", `Resumed ${plural(result.changed)}`);
    });

  return (
    <section className="toolbar-wrap" aria-label="Download controls">
      <div className="toolbar" role="group" aria-label="All downloads">
        <button type="button" className="button button-small button-quiet" disabled={busy} onClick={() => void onPauseAll()}>
          Pause all
        </button>
        <button type="button" className="button button-small button-quiet" disabled={busy} onClick={() => void onResumeAll()}>
          Resume all
        </button>
        <button
          type="button"
          className="button button-small button-quiet"
          aria-expanded={limitsOpen}
          aria-controls="limits-panel"
          onClick={() => setLimitsOpen((open) => !open)}
        >
          Limits
        </button>
      </div>
      {limitsOpen && <LimitsPanel />}
    </section>
  );
}
