// A scripted stand-in for the Tauri commands, built on mockIPC. Records every call so the
// tests can assert which command ran with which arguments.

import { mockIPC } from "@tauri-apps/api/mocks";

import downloadGolden from "../../../../crates/forgedm-protocol/tests/golden/v1/response_GetDownload.json";
import statusGolden from "../../../../crates/forgedm-protocol/tests/golden/v1/response_GetAgentStatus.json";

export type Row = Record<string, unknown>;

export interface Call {
  cmd: string;
  args: Record<string, unknown>;
}

export interface FakeAgent {
  calls: Call[];
  /** Calls of one command, in order. */
  of(cmd: string): Call[];
  /** Replaces the downloads the next snapshot returns. */
  setItems(items: Row[]): void;
  /** Makes one command reject with a command error object (as Tauri does). */
  failWith(cmd: string, error: unknown): void;
}

export const AGENT_STATUS: Row = statusGolden.result as Row;

/** The golden "downloading" row: archive.zip, 1024 of 4096 committed, 512 B/s, ETA 4 s. */
export function goldenRow(overrides: Row = {}): Row {
  const {
    id,
    display_name,
    url_display,
    state,
    priority,
    source,
    target_dir,
    accepted_bytes,
    committed_bytes,
    total_bytes,
    speed_bps,
    eta_ms,
    transfer_mode,
    resume,
    created_at_ms,
  } = downloadGolden.result as Row;
  return {
    id,
    display_name,
    url_display,
    state,
    priority,
    source,
    target_dir,
    accepted_bytes,
    committed_bytes,
    total_bytes,
    speed_bps,
    eta_ms,
    transfer_mode,
    resume,
    created_at_ms,
    ...overrides,
  };
}

const DEFAULT_REPLIES: Record<string, unknown> = {
  enqueue_url: {
    download_id: "018f2c3e-5a10-7b2d-9c4e-0123456789ac",
    state: "queued",
    file_name: "archive.zip",
  },
  pause: { state: "pausing" },
  resume: { state: "queued" },
  cancel: { state: "cancelled" },
  retry: { state: "queued" },
  remove: null,
  show_in_folder: null,
  stop_engine: null,
  start_engine: AGENT_STATUS,
};

export function installFakeAgent(initial: Row[] = []): FakeAgent {
  let items = initial;
  const failures = new Map<string, unknown>();
  const calls: Call[] = [];
  mockIPC((cmd, payload) => {
    calls.push({ cmd, args: (payload ?? {}) as Record<string, unknown> });
    if (failures.has(cmd)) throw failures.get(cmd);
    if (cmd === "get_snapshot") {
      return {
        agent_instance_id: AGENT_STATUS["agent_instance_id"],
        seq: 1,
        agent_status: AGENT_STATUS,
        downloads: items,
        truncated: false,
      };
    }
    if (Object.hasOwn(DEFAULT_REPLIES, cmd)) return DEFAULT_REPLIES[cmd];
    throw new Error(`unexpected command ${cmd}`);
  });
  return {
    calls,
    of: (cmd) => calls.filter((call) => call.cmd === cmd),
    setItems: (next) => {
      items = next;
    },
    failWith: (cmd, error) => {
      failures.set(cmd, error);
    },
  };
}
