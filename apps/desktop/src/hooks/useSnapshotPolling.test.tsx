import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { goldenRow, installFakeAgent } from "../test/fakeAgent";
import { POLL_INTERVAL_MS, useSnapshotPolling } from "./useSnapshotPolling";

function setVisibility(state: "visible" | "hidden"): void {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => state });
  document.dispatchEvent(new Event("visibilitychange"));
}

async function tick(ms: number): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  setVisibility("visible");
});

afterEach(() => {
  vi.useRealTimers();
  setVisibility("visible");
});

describe("useSnapshotPolling", () => {
  it("polls GetSnapshot once at start and then every second", async () => {
    const agent = installFakeAgent([goldenRow()]);
    const { result } = renderHook(() => useSnapshotPolling());
    await tick(0);
    expect(agent.of("get_snapshot")).toHaveLength(1);
    expect(POLL_INTERVAL_MS).toBe(1000);
    await tick(1000);
    await tick(1000);
    expect(agent.of("get_snapshot")).toHaveLength(3);
    expect(result.current.state.phase).toBe("ready");
  });

  it("polling_stops_on_unmount_and_hidden", async () => {
    const agent = installFakeAgent([goldenRow()]);
    const { unmount } = renderHook(() => useSnapshotPolling());
    await tick(0);
    await tick(1000);
    const beforeHide = agent.of("get_snapshot").length;
    expect(beforeHide).toBe(2);

    act(() => setVisibility("hidden"));
    await tick(5000);
    expect(agent.of("get_snapshot")).toHaveLength(beforeHide);

    act(() => setVisibility("visible"));
    await tick(0);
    expect(agent.of("get_snapshot")).toHaveLength(beforeHide + 1);
    await tick(1000);
    expect(agent.of("get_snapshot")).toHaveLength(beforeHide + 2);

    unmount();
    const atUnmount = agent.of("get_snapshot").length;
    await tick(5000);
    expect(agent.of("get_snapshot")).toHaveLength(atUnmount);
  });

  it("never overlaps requests while one is still in flight", async () => {
    const { mockIPC } = await import("@tauri-apps/api/mocks");
    let started = 0;
    mockIPC(
      () =>
        new Promise(() => {
          started += 1;
        }),
    );
    renderHook(() => useSnapshotPolling());
    await tick(5000);
    expect(started).toBe(1);
  });

  it("reports an unavailable engine and recovers when it returns", async () => {
    const agent = installFakeAgent();
    agent.failWith("get_snapshot", { kind: "agent_unavailable", message: "The background engine is not running." });
    const { result } = renderHook(() => useSnapshotPolling());
    await tick(0);
    expect(result.current.state.phase).toBe("unavailable");
    installFakeAgent([goldenRow()]);
    await tick(1000);
    expect(result.current.state.phase).toBe("ready");
  });

  it("malformed_payload_error_state", async () => {
    const { mockIPC } = await import("@tauri-apps/api/mocks");
    mockIPC(() => ({ nonsense: true }));
    const { result } = renderHook(() => useSnapshotPolling());
    await tick(0);
    expect(result.current.state).toMatchObject({ phase: "error", title: "Unexpected answer from the engine" });
  });
});
