// One typed wrapper per Tauri command. Argument names are the snake_case names the Rust
// commands declare; optional values are omitted rather than sent as null.

import {
  PayloadError,
  call,
  optUint,
  parseAgentStatus,
  parseSnapshot,
  type AgentStatusInfo,
  type EngineSnapshot,
  asObject,
  str,
  type Obj,
} from "./ipc";
import { parseLimitsResult, type EffectiveLimits, type LimitsInput } from "./limits";

export interface EnqueueInput {
  url: string;
  targetDir?: string;
  sha256?: string;
}

export interface EnqueueResult {
  download_id: string;
  state: string;
  file_name: string;
}

function parseEnqueueResult(value: unknown): EnqueueResult {
  const o = asObject(value, "enqueue result");
  return { download_id: str(o, "download_id"), state: str(o, "state"), file_name: str(o, "file_name") };
}

const ignore = (): void => undefined;

export function getSnapshot(): Promise<EngineSnapshot> {
  return call("get_snapshot", parseSnapshot);
}

export function enqueueUrl(input: EnqueueInput): Promise<EnqueueResult> {
  const args: Record<string, unknown> = { url: input.url };
  if (input.targetDir !== undefined) args["target_dir"] = input.targetDir;
  if (input.sha256 !== undefined) args["sha256"] = input.sha256;
  return call("enqueue_url", parseEnqueueResult, args);
}

export function pauseDownload(id: string): Promise<void> {
  return call("pause", ignore, { id, discard_progress: false });
}

export function resumeDownload(id: string): Promise<void> {
  return call("resume", ignore, { id });
}

export function cancelDownload(id: string): Promise<void> {
  return call("cancel", ignore, { id });
}

export function retryDownload(id: string, restartFromZero: boolean): Promise<void> {
  return call("retry", ignore, { id, restart_from_zero: restartFromZero });
}

export function removeDownload(id: string, deletePartial: boolean): Promise<void> {
  return call("remove", ignore, { id, delete_partial: deletePartial });
}

export function showInFolder(id: string): Promise<void> {
  return call("show_in_folder", ignore, { id });
}

export function startEngine(): Promise<AgentStatusInfo> {
  return call("start_engine", parseAgentStatus);
}

export function stopEngine(): Promise<void> {
  return call("stop_engine", ignore);
}

/** `PauseAll` / `ResumeAll` answer: how many downloads changed and how many were skipped. */
export interface BatchResult {
  changed: number;
  skipped: number;
}

function idCount(obj: Obj, key: string): number {
  const value = obj[key];
  if (!Array.isArray(value)) throw new PayloadError(`${key} is not an array`);
  return value.length;
}

function parsePauseAll(value: unknown): BatchResult {
  const o = asObject(value, "pause-all result");
  return { changed: idCount(o, "paused"), skipped: idCount(o, "skipped_not_resumable") };
}

function parseResumeAll(value: unknown): BatchResult {
  return { changed: idCount(asObject(value, "resume-all result"), "resumed"), skipped: 0 };
}

export function pauseAll(discardProgress = false): Promise<BatchResult> {
  return call("pause_all", parsePauseAll, { discard_progress: discardProgress });
}

export function resumeAll(): Promise<BatchResult> {
  return call("resume_all", parseResumeAll);
}

/** The global limits in force, read without changing anything. */
export function getLimits(): Promise<EffectiveLimits> {
  return call("get_limits", parseLimitsResult);
}

export function updateGlobalLimits(input: LimitsInput): Promise<EffectiveLimits> {
  return call("update_limits", parseLimitsResult, { scope: "global", ...input });
}

export function updateDownloadConnections(id: string, maxConnections: number): Promise<EffectiveLimits> {
  return call("update_limits", parseLimitsResult, {
    scope: "download",
    download_id: id,
    max_connections: maxConnections,
  });
}

export function updatePriority(id: string, priority: string): Promise<void> {
  return call("update_priority", ignore, { id, priority });
}

/** The time of the next automatic retry from `GetDownload`, when the engine has set one. */
export function getNextRetryAt(id: string): Promise<number | undefined> {
  return call("get_download", (value) => optUint(asObject(value, "download"), "next_retry_at_ms"), { id });
}
