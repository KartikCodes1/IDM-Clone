import { useEffect, useRef, useState } from "react";

import { ACTION_LABELS, actionsFor, failureText, type RowAction } from "../lib/actions";
import { formatBytes, formatEta, formatSpeed, hostOf, percentComplete, stateLabel } from "../lib/format";
import type { DownloadSummary } from "../lib/ipc";

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

interface RowProps {
  item: DownloadSummary;
  onRequest: (item: DownloadSummary, request: RowRequest) => void;
}

/** One download. Everything shown from the engine is rendered as text. */
export function DownloadRow({ item, onRequest }: RowProps) {
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
      {item.state === "failed" && <p className="row-failure">{failureText(item.last_error)}</p>}
    </li>
  );
}
