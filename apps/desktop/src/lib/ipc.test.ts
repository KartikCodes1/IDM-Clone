import { describe, expect, it } from "vitest";

import downloadListGolden from "../../../../crates/forgedm-protocol/tests/golden/v1/response_ListDownloads.json";
import statusGolden from "../../../../crates/forgedm-protocol/tests/golden/v1/response_GetAgentStatus.json";
import { PayloadError, parseAgentStatus, parseCommandError, parseDownloadList } from "./ipc";

describe("ipc guards", () => {
  it("parseAgentStatus_accepts_golden_rejects_malformed", () => {
    const status = parseAgentStatus(statusGolden.result);
    expect(status.agent_version).toBe("0.1.0");
    expect(status.agent_pid).toBe(4321);
    expect(status.state).toBe("ready");
    expect(status.active).toBe(1);
    expect(status.total_speed_bps).toBe(1048576);

    const golden = statusGolden.result as Record<string, unknown>;
    const malformed: unknown[] = [
      null,
      undefined,
      "ready",
      42,
      [],
      {},
      { ...golden, agent_pid: "4321" },
      { ...golden, agent_pid: -1 },
      { ...golden, agent_pid: 1.5 },
      { ...golden, uptime_ms: Number.MAX_SAFE_INTEGER + 2 },
      { ...golden, state: 7 },
      { ...golden, data_dir: null },
      Object.fromEntries(Object.entries(golden).filter(([key]) => key !== "agent_instance_id")),
    ];
    for (const value of malformed) {
      expect(() => parseAgentStatus(value), JSON.stringify(value)).toThrow(PayloadError);
    }
    // Clients are tolerant of unknown fields and unknown states (Appendix A.1).
    const tolerant = parseAgentStatus({ ...golden, future_field: 1, state: "hibernating" });
    expect(tolerant.state).toBe("hibernating");
  });

  it("parseDownloadList_accepts_golden", () => {
    const list = parseDownloadList(downloadListGolden.result);
    expect(list.total).toBe(1);
    const [item] = list.items;
    expect(item?.display_name).toBe("archive.zip");
    expect(item?.total_bytes).toBe(4096);
    expect(item?.speed_bps).toBe(512);
    expect(item?.final_path).toBeUndefined();

    expect(() => parseDownloadList({ items: "nope", total: 0 })).toThrow(PayloadError);
    expect(() => parseDownloadList({ items: [{ id: 1 }], total: 1 })).toThrow(PayloadError);
    expect(() => parseDownloadList({ items: [], total: -1 })).toThrow(PayloadError);
    expect(parseDownloadList({ items: [], total: 0 }).items).toEqual([]);
  });

  it("parseCommandError_reads_kind_and_message_and_falls_back", () => {
    expect(parseCommandError({ kind: "agent_unavailable", message: "m" })).toEqual({
      kind: "agent_unavailable",
      message: "m",
    });
    expect(parseCommandError("boom").kind).toBe("internal");
    expect(parseCommandError({ kind: 3 }).kind).toBe("internal");
    expect(parseCommandError({ kind: "brand_new", message: "x" }).kind).toBe("internal");
  });
});
