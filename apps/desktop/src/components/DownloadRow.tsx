import { useEffect, useRef, useState } from "react";

import { useRetryAt } from "../hooks/useRetryAt";
import type { RowSetting } from "../hooks/useRowSettings";
import { ACTION_LABELS, actionsFor, type RowAction } from "../lib/actions";
import { formatBytes, formatEta, formatSpeed, hostOf, percentComplete, stateLabel } from "../lib/format";
import type { DownloadSummary } from "../lib/ipc";
import { MAX_CONNECTIONS } from "../lib/limits";
import { stateMessage } from "../lib/messages";

/** What a row asks the app to do; `deletePartial` only matters for `remove`. */
export interface RowRequest {
  action: RowAction;
  deletePartial?: boolean;
}

function Progress({ item }: { item: DownloadSummary }) {
  const percent = percentComplete(item.committed_bytes, item.total_bytes);
  if (percent === undefined) {
    return (
      <div className="progress-cell">
        <div className="progress progress-indeterminate" role="progressbar" aria-label={`Progress of ${item.display_name} unknown`} />
        <span className="muted">Unknown size</span>
      </div>
    );
  }
  return (
    <div className="progress-cell">
      <div className="progress" role="progressbar" aria-label={`Progress of ${item.display_name}`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent}>
        <div className="progress-fill" ref={(el) => el?.style.setProperty("--fill", `${percent}%`)} />
      </div>
      <span className="num">{percent}%</span>
    </div>
  );
}

function Stats({ item }: { item: DownloadSummary }) {
  const speed = formatSpeed(item.speed_bps);
  const eta = formatEta(item);
  return (
    <span className="num muted row-size">
      {formatBytes(item.committed_bytes)}
      {item.total_bytes === undefined ? "" : ` of ${formatBytes(item.total_bytes)}`}
      {speed === "" ? "" : ` · ${speed}`}
      {eta === "" ? null : <span className="row-eta">{`ETA ${eta}`}</span>}
    </span>
  );
}

function RemoveConfirm(props: { completed: boolean; onChoose: (request: RowRequest | undefined) => void }) {
  const group = useRef<HTMLDivElement>(null);
  useEffect(() => group.current?.querySelector("button")?.focus(), []);
  const remove = (deletePartial: boolean): RowRequest => ({ action: "remove", deletePartial });
  return (
    <div ref={group} className="row-actions" role="group" aria-label="Confirm removal">
      <span className="muted">Remove this download?</span>
      <button type="button" className="button button-small" onClick={() => props.onChoose(remove(false))}>
        Remove from list
      </button>
      {!props.completed && (
        <button type="button" className="button button-danger button-small" onClick={() => props.onChoose(remove(true))}>
          Remove and delete partial file
        </button>
      )}
      <button type="button" className="button button-quiet button-small" onClick={() => props.onChoose(undefined)}>
        Keep download
      </button>
    </div>
  );
}

const PRIORITIES = [
  ["high", "High"],
  ["normal", "Normal"],
  ["low", "Low"],
] as const;

const TUNABLE_STATES: readonly string[] = ["queued", "probing", "downloading", "pausing", "paused", "retry_waiting", "interrupted"];

const CONNECTION_CHOICES = Array.from({ length: MAX_CONNECTIONS }, (_, i) => i + 1);

/** Priority and connection-count selectors for downloads that are still going to run. */
function RowSettings(props: { item: DownloadSummary; onSetting: (item: DownloadSummary, setting: RowSetting) => void }) {
  const { item, onSetting } = props;
  const priority = PRIORITIES.some(([value]) => value === item.priority) ? item.priority : "";
  return (
    <div className="row-settings">
      <select
        className="row-select"
        aria-label={`Priority for ${item.display_name}`}
        value={priority}
        onChange={(event) => onSetting(item, { kind: "priority", value: event.target.value })}
      >
        {priority === "" && (
          <option value="" disabled>
            Priority
          </option>
        )}
        {PRIORITIES.map(([value, label]) => (
          <option key={value} value={value}>
            {label}
          </option>
        ))}
      </select>
      <select
        className="row-select"
        aria-label={`Connections for ${item.display_name}`}
        value=""
        onChange={(event) => onSetting(item, { kind: "connections", value: Number(event.target.value) })}
      >
        <option value="" disabled>
          Connections
        </option>
        {CONNECTION_CHOICES.map((count) => (
          <option key={count} value={count}>
            {count}
          </option>
        ))}
      </select>
    </div>
  );
}

interface RowProps {
  item: DownloadSummary;
  onRequest: (item: DownloadSummary, request: RowRequest) => void;
  onSetting: (item: DownloadSummary, setting: RowSetting) => void;
}

/** One download. Everything shown from the engine is rendered as text. */
export function DownloadRow({ item, onRequest, onSetting }: RowProps) {
  const retryAt = useRetryAt(item);
  const note = stateMessage(item, retryAt);
  const [confirming, setConfirming] = useState(false);
  const row = useRef<HTMLLIElement>(null);
  const wasConfirming = useRef(false);

  // The confirmation replaces the focused button; give focus back to Remove when it closes.
  useEffect(() => {
    if (wasConfirming.current && !confirming) row.current?.querySelector<HTMLElement>('[data-action="remove"]')?.focus();
    wasConfirming.current = confirming;
  }, [confirming]);

  const choose = (request: RowRequest | undefined) => {
    setConfirming(false);
    if (request !== undefined) onRequest(item, request);
  };
  const click = (action: RowAction) => {
    if (action === "remove") setConfirming(true);
    else onRequest(item, { action });
  };

  return (
    <li ref={row} className="row">
      <div className="row-head">
        <div className="row-name">
          <span className="row-title">{item.display_name}</span>
          <span className="row-sub muted">{hostOf(item.url_display)}</span>
        </div>
        <span className={`chip chip-${stateLabel(item.state) === "Unknown" ? "unknown" : item.state}`}>{stateLabel(item.state)}</span>
        {confirming ? (
          <RemoveConfirm completed={item.state === "completed"} onChoose={choose} />
        ) : (
          <div className="row-actions">
            {TUNABLE_STATES.includes(item.state) && <RowSettings item={item} onSetting={onSetting} />}
            {actionsFor(item.state).map((action) => (
              <button key={action} type="button" data-action={action} className="button button-small button-quiet" aria-label={`${ACTION_LABELS[action]} ${item.display_name}`} onClick={() => click(action)}>
                {ACTION_LABELS[action]}
              </button>
            ))}
          </div>
        )}
      </div>
      <div className="row-body">
        <Progress item={item} />
        <Stats item={item} />
      </div>
      {note !== undefined && <p className={item.state === "failed" ? "row-failure" : "row-note"}>{note}</p>}
    </li>
  );
}
