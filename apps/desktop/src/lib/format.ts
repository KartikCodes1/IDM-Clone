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

/** Compact duration: `4s`, `2m 05s`, `1h 02m`, `1d 1h`. */
export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000));
  const days = Math.floor(total / 86_400);
  const hours = Math.floor((total % 86_400) / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${String(minutes).padStart(2, "0")}m`;
  if (minutes > 0) return `${minutes}m ${String(seconds).padStart(2, "0")}s`;
  return `${seconds}s`;
}

const ETA_STATES: readonly string[] = ["probing", "downloading"];

/**
 * ETA text for a row. A real duration only when the engine supplied one; otherwise
 * "Estimating..." (size known, speed not yet) or "Unknown" (size unknown). Inactive
 * downloads show nothing.
 */
export function formatEta(item: { state: string; total_bytes?: number; eta_ms?: number }): string {
  if (!ETA_STATES.includes(item.state)) return "";
  if (item.eta_ms !== undefined) return formatDuration(item.eta_ms);
  return item.total_bytes === undefined ? "Unknown" : "Estimating...";
}

/** The host part of a redacted URL; anything unparsable is returned as given (text only). */
export function hostOf(urlDisplay: string): string {
  try {
    return new URL(urlDisplay).host || urlDisplay;
  } catch {
    return urlDisplay;
  }
}
