// Typed, runtime-validated access to the Tauri commands (ADR-022).
// Every payload is checked by hand-written guards; the golden JSON files from
// forgedm-protocol are used by the tests. Clients are tolerant: unknown fields and
// unknown enum values are accepted (PHASE_0_1_PLAN A.1).

import { invoke } from "@tauri-apps/api/core";

/** A payload from the engine did not have the expected shape. */
export class PayloadError extends Error {
  constructor(what: string) {
    super(`invalid payload: ${what}`);
    this.name = "PayloadError";
  }
}

export interface AgentStatusInfo {
  agent_version: string;
  agent_pid: number;
  agent_instance_id: string;
  state: string;
  uptime_ms: number;
  active: number;
  queued: number;
  total_speed_bps: number;
  proxy_source: string;
  data_dir: string;
}

/** A download failure as the engine reports it; the code set is open (newer engines). */
export interface DownloadFailure {
  code: string;
  message: string;
  retryable: boolean;
  /** Parameters of the code; only the retry time is used by the UI. */
  details?: { retry_at_ms?: number };
}

export interface DownloadSummary {
  id: string;
  display_name: string;
  url_display: string;
  state: string;
  priority: string;
  source: string;
  target_dir: string;
  accepted_bytes: number;
  committed_bytes: number;
  created_at_ms: number;
  resume: string;
  total_bytes?: number;
  speed_bps?: number;
  eta_ms?: number;
  state_reason?: string;
  final_path?: string;
  transfer_mode?: string;
  completed_at_ms?: number;
  last_error?: DownloadFailure;
}

export interface DownloadList {
  items: DownloadSummary[];
  total: number;
}

/** `GetSnapshot`: engine status and the download list in one consistent answer. */
export interface EngineSnapshot {
  agent_status: AgentStatusInfo;
  downloads: DownloadSummary[];
  truncated: boolean;
  seq: number;
}

export type CommandErrorKind =
  | "invalid_request"
  | "agent_unavailable"
  | "agent_identity"
  | "protocol_mismatch"
  | "agent_error"
  | "internal";

export interface CommandError {
  kind: CommandErrorKind;
  message: string;
}

const COMMAND_ERROR_KINDS: readonly string[] = [
  "invalid_request",
  "agent_unavailable",
  "agent_identity",
  "protocol_mismatch",
  "agent_error",
  "internal",
];

export type Obj = Record<string, unknown>;

export function asObject(value: unknown, what: string): Obj {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new PayloadError(`${what} is not an object`);
  }
  return value as Obj;
}

export function str(obj: Obj, key: string): string {
  const value = obj[key];
  if (typeof value !== "string") throw new PayloadError(`${key} is not a string`);
  return value;
}

/** Non-negative integer that survives a JS number (the protocol keeps u64 below 2^53). */
export function uint(obj: Obj, key: string): number {
  const value = obj[key];
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new PayloadError(`${key} is not a non-negative safe integer`);
  }
  return value;
}

export function optUint(obj: Obj, key: string): number | undefined {
  return obj[key] === undefined || obj[key] === null ? undefined : uint(obj, key);
}

function optStr(obj: Obj, key: string): string | undefined {
  return obj[key] === undefined || obj[key] === null ? undefined : str(obj, key);
}

export function parseAgentStatus(value: unknown): AgentStatusInfo {
  const o = asObject(value, "agent status");
  return {
    agent_version: str(o, "agent_version"),
    agent_pid: uint(o, "agent_pid"),
    agent_instance_id: str(o, "agent_instance_id"),
    state: str(o, "state"),
    uptime_ms: uint(o, "uptime_ms"),
    active: uint(o, "active"),
    queued: uint(o, "queued"),
    total_speed_bps: uint(o, "total_speed_bps"),
    proxy_source: str(o, "proxy_source"),
    data_dir: str(o, "data_dir"),
  };
}

function withOptional<K extends keyof DownloadSummary>(
  key: K,
  value: DownloadSummary[K] | undefined,
): Partial<DownloadSummary> {
  return value === undefined ? {} : { [key]: value };
}

function parseFailure(value: unknown): DownloadFailure {
  const o = asObject(value, "last_error");
  const retryable = o["retryable"];
  const details = o["details"];
  const retryAt =
    typeof details === "object" && details !== null && !Array.isArray(details)
      ? optUint(details as Obj, "retry_at_ms")
      : undefined;
  return {
    code: str(o, "code"),
    message: str(o, "message"),
    retryable: typeof retryable === "boolean" ? retryable : false,
    ...(retryAt === undefined ? {} : { details: { retry_at_ms: retryAt } }),
  };
}

function parseDownloadSummary(value: unknown): DownloadSummary {
  const o = asObject(value, "download");
  return {
    id: str(o, "id"),
    display_name: str(o, "display_name"),
    url_display: str(o, "url_display"),
    state: str(o, "state"),
    priority: str(o, "priority"),
    source: str(o, "source"),
    target_dir: str(o, "target_dir"),
    accepted_bytes: uint(o, "accepted_bytes"),
    committed_bytes: uint(o, "committed_bytes"),
    created_at_ms: uint(o, "created_at_ms"),
    resume: str(o, "resume"),
    ...withOptional("total_bytes", optUint(o, "total_bytes")),
    ...withOptional("speed_bps", optUint(o, "speed_bps")),
    ...withOptional("eta_ms", optUint(o, "eta_ms")),
    ...withOptional("completed_at_ms", optUint(o, "completed_at_ms")),
    ...withOptional("state_reason", optStr(o, "state_reason")),
    ...withOptional("final_path", optStr(o, "final_path")),
    ...withOptional("transfer_mode", optStr(o, "transfer_mode")),
    ...withOptional(
      "last_error",
      o["last_error"] === undefined || o["last_error"] === null ? undefined : parseFailure(o["last_error"]),
    ),
  };
}

export function parseDownloadList(value: unknown): DownloadList {
  const o = asObject(value, "download list");
  const items = o["items"];
  if (!Array.isArray(items)) throw new PayloadError("items is not an array");
  return { items: items.map(parseDownloadSummary), total: uint(o, "total") };
}

export function parseSnapshot(value: unknown): EngineSnapshot {
  const o = asObject(value, "snapshot");
  const downloads = o["downloads"];
  if (!Array.isArray(downloads)) throw new PayloadError("downloads is not an array");
  if (typeof o["truncated"] !== "boolean") throw new PayloadError("truncated is not a boolean");
  return {
    agent_status: parseAgentStatus(o["agent_status"]),
    downloads: downloads.map(parseDownloadSummary),
    truncated: o["truncated"],
    seq: uint(o, "seq"),
  };
}

/** Normalises whatever a rejected `invoke` carries into a [`CommandError`]. */
export function parseCommandError(value: unknown): CommandError {
  if (typeof value === "object" && value !== null) {
    const { kind, message } = value as Obj;
    if (typeof kind === "string" && COMMAND_ERROR_KINDS.includes(kind)) {
      return {
        kind: kind as CommandErrorKind,
        message: typeof message === "string" ? message : "",
      };
    }
  }
  return { kind: "internal", message: "Communication with the background engine failed." };
}

/** Raised by the fetch helpers: either a command failure or a payload failure. */
export class CommandFailure extends Error {
  readonly error: CommandError;
  constructor(error: CommandError) {
    super(error.message);
    this.name = "CommandFailure";
    this.error = error;
  }
}

/** Invokes `command` and validates the answer with `parse`. */
export async function call<T>(
  command: string,
  parse: (value: unknown) => T,
  args?: Record<string, unknown>,
): Promise<T> {
  let raw: unknown;
  try {
    raw = await invoke<unknown>(command, args);
  } catch (err) {
    throw new CommandFailure(parseCommandError(err));
  }
  return parse(raw);
}

export function getAgentStatus(): Promise<AgentStatusInfo> {
  return call("agent_status", parseAgentStatus);
}

export function listDownloads(): Promise<DownloadList> {
  return call("list_downloads", parseDownloadList);
}
