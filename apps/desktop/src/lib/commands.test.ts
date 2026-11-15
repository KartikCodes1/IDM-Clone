import { afterEach, describe, expect, it, vi } from "vitest";

import { AGENT_STATUS, goldenRow, installFakeAgent } from "../test/fakeAgent";
import {
  cancelDownload,
  enqueueUrl,
  getSnapshot,
  pauseDownload,
  removeDownload,
  resumeDownload,
  retryDownload,
  showInFolder,
  startEngine,
  stopEngine,
} from "./commands";
import { CommandFailure, PayloadError } from "./ipc";

const ID = "018f2c3e-5a10-7b2d-9c4e-0123456789ab";

afterEach(() => {
  vi.restoreAllMocks();
});

describe("commands", () => {
  it("getSnapshot_parses_the_engine_snapshot", async () => {
    installFakeAgent([goldenRow()]);
    const snapshot = await getSnapshot();
    expect(snapshot.agent_status.agent_version).toBe("0.1.0");
    expect(snapshot.downloads).toHaveLength(1);
    expect(snapshot.downloads[0]?.display_name).toBe("archive.zip");
    expect(snapshot.truncated).toBe(false);
  });

  it("malformed_snapshot_is_a_payload_error", async () => {
    installFakeAgent();
    const { mockIPC } = await import("@tauri-apps/api/mocks");
    mockIPC(() => ({ agent_status: AGENT_STATUS, downloads: "nope" }));
    await expect(getSnapshot()).rejects.toBeInstanceOf(PayloadError);
  });

  it("enqueue_sends_snake_case_args_and_omits_empty_options", async () => {
    const agent = installFakeAgent();
    const result = await enqueueUrl({ url: "https://example.com/a.zip" });
    expect(agent.of("enqueue_url")[0]?.args).toEqual({ url: "https://example.com/a.zip" });
    expect(result).toEqual({ download_id: "018f2c3e-5a10-7b2d-9c4e-0123456789ac", state: "queued", file_name: "archive.zip" });
    await enqueueUrl({ url: "https://example.com/b", targetDir: String.raw`C:\Dl`, sha256: "ab".repeat(32) });
    expect(agent.of("enqueue_url")[1]?.args).toEqual({
      url: "https://example.com/b",
      target_dir: String.raw`C:\Dl`,
      sha256: "ab".repeat(32),
    });
  });

  it("each_row_command_sends_the_id_and_flags_the_rust_side_expects", async () => {
    const agent = installFakeAgent();
    await pauseDownload(ID);
    await resumeDownload(ID);
    await cancelDownload(ID);
    await retryDownload(ID, true);
    await removeDownload(ID, true);
    await showInFolder(ID);
    expect(agent.calls.map((c) => [c.cmd, c.args])).toEqual([
      ["pause", { id: ID, discard_progress: false }],
      ["resume", { id: ID }],
      ["cancel", { id: ID }],
      ["retry", { id: ID, restart_from_zero: true }],
      ["remove", { id: ID, delete_partial: true }],
      ["show_in_folder", { id: ID }],
    ]);
  });

  it("start_and_stop_engine_call_their_commands", async () => {
    const agent = installFakeAgent();
    const status = await startEngine();
    await stopEngine();
    expect(status.state).toBe("ready");
    expect(agent.calls.map((c) => c.cmd)).toEqual(["start_engine", "stop_engine"]);
  });

  it("command_failures_carry_the_friendly_message", async () => {
    const agent = installFakeAgent();
    agent.failWith("pause", { kind: "agent_error", message: "That download no longer exists." });
    const error = await pauseDownload(ID).catch((err: unknown) => err);
    expect(error).toBeInstanceOf(CommandFailure);
    expect((error as CommandFailure).error).toEqual({
      kind: "agent_error",
      message: "That download no longer exists.",
    });
  });
});
