import { formatSpeed } from "../lib/format";
import type { AgentStatusInfo } from "../lib/ipc";

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="stat">
      <dt className="muted">{label}</dt>
      <dd className="num">{value}</dd>
    </div>
  );
}

export function StatusStrip({ status }: { status: AgentStatusInfo }) {
  return (
    <dl className="strip" aria-label="Engine status">
      <Stat label="Active" value={String(status.active)} />
      <Stat label="Queued" value={String(status.queued)} />
      <Stat label="Speed" value={formatSpeed(status.total_speed_bps) || "0 B/s"} />
      <Stat label="Engine version" value={status.agent_version} />
    </dl>
  );
}
