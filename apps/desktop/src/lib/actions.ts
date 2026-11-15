// Which row commands are legal in which download state (PLAN Appendix A.6). The engine
// still enforces its own state machine; this only avoids offering buttons that must fail.

export type RowAction = "pause" | "resume" | "cancel" | "retry" | "remove" | "show";

const BY_STATE: Readonly<Record<string, readonly RowAction[]>> = {
  queued: ["pause", "cancel", "remove"],
  probing: ["pause", "cancel", "remove"],
  downloading: ["pause", "cancel", "remove"],
  pausing: ["cancel", "remove"],
  paused: ["resume", "cancel", "remove"],
  retry_waiting: ["resume", "pause", "cancel", "remove"],
  verifying: ["cancel", "remove"],
  finalizing: [],
  completed: ["show", "remove"],
  failed: ["retry", "remove"],
  cancelled: ["retry", "remove"],
  interrupted: ["resume", "cancel", "remove"],
  needs_credentials: ["retry", "cancel", "remove"],
  needs_url_refresh: ["retry", "cancel", "remove"],
};

/** Unknown states (a newer engine) allow only removal. */
export function actionsFor(state: string): readonly RowAction[] {
  return Object.hasOwn(BY_STATE, state) ? (BY_STATE[state] ?? ["remove"]) : ["remove"];
}

export const ACTION_LABELS: Readonly<Record<RowAction, string>> = {
  pause: "Pause",
  resume: "Resume",
  cancel: "Cancel",
  retry: "Retry",
  remove: "Remove",
  show: "Show in folder",
};

const UNTRUSTED_PARTIAL: readonly string[] = [
  "checksum_mismatch",
  "size_mismatch",
  "remote_changed",
  "part_file_invalid",
];

/** After these failures the partial data cannot be trusted, so Retry starts from zero. */
export function retryRestartsFromZero(failure: { code: string } | undefined): boolean {
  return failure !== undefined && UNTRUSTED_PARTIAL.includes(failure.code);
}

const FAILURE_TEXT: Readonly<Record<string, string>> = {
  network_timeout: "The connection timed out.",
  network_connect: "Could not connect to the server.",
  network_reset: "The connection was reset.",
  dns_failure: "The server name could not be found.",
  rate_limited: "The server asked us to slow down.",
  http_status: "The server refused the download.",
  auth_required: "The server needs a sign-in.",
  url_expired: "The link has expired.",
  tls_failure: "A secure connection could not be established.",
  checksum_mismatch: "The file does not match the SHA-256 you entered.",
  size_mismatch: "The file size does not match what the server announced.",
  remote_changed: "The file changed on the server.",
  disk_full: "The disk is full.",
  destination_unavailable: "The destination folder is unavailable.",
  disk_io: "Writing to the disk failed.",
  path_too_long: "The file path is too long.",
  blocked_by_antivirus: "Security software blocked the file.",
  blocked_by_policy: "A policy blocked the file.",
  finalize_locked: "The finished file is locked by another program.",
  part_file_invalid: "The partial file went missing.",
  retry_budget_exhausted: "Too many attempts failed.",
};

/** A fixed, readable reason for a failed download. The engine's own text is not shown. */
export function failureText(failure: { code: string } | undefined): string {
  if (failure === undefined) return "The download failed.";
  return Object.hasOwn(FAILURE_TEXT, failure.code)
    ? (FAILURE_TEXT[failure.code] ?? "The download failed.")
    : "The download failed.";
}
