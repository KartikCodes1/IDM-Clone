import { renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { DownloadSummary } from "../lib/ipc";
import { goldenRow, installFakeAgent } from "../test/fakeAgent";
import { useRetryAt } from "./useRetryAt";

const FIRST = new Date(2026, 10, 16, 23, 45, 0).getTime();
const SECOND = FIRST + 3_600_000;

function row(state: string): DownloadSummary {
  return goldenRow({ state }) as unknown as DownloadSummary;
}

describe("useRetryAt", () => {
  it("a_new_wait_never_shows_the_previous_wait_time", async () => {
    const agent = installFakeAgent([goldenRow({ state: "retry_waiting" })]);
    agent.reply("get_download", { ...goldenRow({ state: "retry_waiting" }), next_retry_at_ms: FIRST });
    const { result, rerender } = renderHook(({ item }) => useRetryAt(item), {
      initialProps: { item: row("retry_waiting") },
    });
    await waitFor(() => expect(result.current).toBe(FIRST));

    rerender({ item: row("downloading") });
    expect(result.current).toBeUndefined();

    agent.reply("get_download", { ...goldenRow({ state: "retry_waiting" }), next_retry_at_ms: SECOND });
    rerender({ item: row("retry_waiting") });
    expect(result.current).toBeUndefined();
    await waitFor(() => expect(result.current).toBe(SECOND));
    expect(agent.of("get_download")).toHaveLength(2);
  });
});
