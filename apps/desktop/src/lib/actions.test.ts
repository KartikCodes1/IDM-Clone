import { describe, expect, it } from "vitest";

import { actionsFor, retryRestartsFromZero } from "./actions";

describe("actionsFor", () => {
  it("offers exactly the legal commands per state (PLAN A.6)", () => {
    const table: Record<string, string[]> = {
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
    for (const [state, expected] of Object.entries(table)) {
      expect(actionsFor(state), state).toEqual(expected);
    }
  });

  it("allows only removal for an unknown state from a newer engine", () => {
    expect(actionsFor("teleporting")).toEqual(["remove"]);
  });

  it("restarts from zero only when the partial data can no longer be trusted", () => {
    for (const code of ["checksum_mismatch", "size_mismatch", "remote_changed", "part_file_invalid"]) {
      expect(retryRestartsFromZero({ code }), code).toBe(true);
    }
    for (const code of ["network_timeout", "disk_full", "http_status"]) {
      expect(retryRestartsFromZero({ code }), code).toBe(false);
    }
    expect(retryRestartsFromZero(undefined)).toBe(false);
  });
});
