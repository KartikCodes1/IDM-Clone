import { formatBytes, formatSpeed, percentComplete, stateLabel } from "../lib/format";
import type { DownloadSummary } from "../lib/ipc";

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
      <div
        className="progress"
        role="progressbar"
        aria-label={`Progress of ${item.display_name}`}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={percent}
      >
        <div className="progress-fill" ref={(el) => el?.style.setProperty("--fill", `${percent}%`)} />
      </div>
      <span className="num">{percent}%</span>
    </div>
  );
}

function Row({ item }: { item: DownloadSummary }) {
  const speed = formatSpeed(item.speed_bps);
  return (
    <li className="row">
      <div className="row-name">
        <span className="row-title">{item.display_name}</span>
        <span className="row-sub muted">{item.url_display}</span>
      </div>
      <span className="chip">{stateLabel(item.state)}</span>
      <Progress item={item} />
      <span className="num muted row-size">
        {formatBytes(item.committed_bytes)}
        {item.total_bytes === undefined ? "" : ` of ${formatBytes(item.total_bytes)}`}
        {speed === "" ? "" : ` · ${speed}`}
      </span>
    </li>
  );
}

export function DownloadList({ items }: { items: DownloadSummary[] }) {
  return (
    <ul className="list" aria-label="Downloads">
      {items.map((item) => (
        <Row key={item.id} item={item} />
      ))}
    </ul>
  );
}
