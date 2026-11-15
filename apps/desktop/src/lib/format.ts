const UNITS = ["B", "KiB", "MiB", "GiB", "TiB"] as const;

export function formatBytes(bytes: number): string {
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${value} B` : `${value.toFixed(1)} ${UNITS[unit]}`;
}

/** Empty string when the speed is unknown: never a made-up number. */
export function formatSpeed(bps: number | undefined): string {
  return bps === undefined ? "" : `${formatBytes(bps)}/s`;
}

/** A whole-number percentage, or undefined when the total is unknown or zero. */
export function percentComplete(done: number, total: number | undefined): number | undefined {
  if (total === undefined || total === 0) return undefined;
  return Math.min(100, Math.floor((done / total) * 100));
}

const STATE_LABELS: Readonly<Record<string, string>> = {
  queued: "Queued",
  probing: "Checking",
  downloading: "Downloading",
  pausing: "Pausing",
  paused: "Paused",
  retry_waiting: "Waiting to retry",
  verifying: "Verifying",
  finalizing: "Finishing",
  completed: "Completed",
  failed: "Failed",
  cancelled: "Cancelled",
  interrupted: "Interrupted",
  needs_credentials: "Needs sign-in",
  needs_url_refresh: "Needs new link",
};

/** Unknown states (a newer agent) are shown as "Unknown" (Appendix A.1). */
export function stateLabel(state: string): string {
  return Object.hasOwn(STATE_LABELS, state) ? (STATE_LABELS[state] ?? "Unknown") : "Unknown";
}
