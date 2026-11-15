// One typed wrapper per Tauri command. Argument names are the snake_case names the Rust
// commands declare; optional values are omitted rather than sent as null.

import {
  call,
  parseAgentStatus,
  parseSnapshot,
  type AgentStatusInfo,
  type EngineSnapshot,
  asObject,
  str,
} from "./ipc";

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
