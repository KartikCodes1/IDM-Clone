import { describe, expect, it } from "vitest";

import { formatBytes, formatDuration, formatEta, formatSpeed, percentComplete, stateLabel } from "./format";

describe("format", () => {
  it("formats bytes with binary units", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1023)).toBe("1023 B");
    expect(formatBytes(1024)).toBe("1.0 KiB");
    expect(formatBytes(5 * 1024 * 1024 * 1024)).toBe("5.0 GiB");
  });

  it("formats speed and leaves unknown speed blank", () => {
    expect(formatSpeed(2048)).toBe("2.0 KiB/s");
    expect(formatSpeed(undefined)).toBe("");
  });

  it("computes a percentage only when the total is known and non-zero", () => {
    expect(percentComplete(1024, 4096)).toBe(25);
    expect(percentComplete(1024, undefined)).toBeUndefined();
    expect(percentComplete(0, 0)).toBeUndefined();
    expect(percentComplete(5000, 4096)).toBe(100);
  });

  it("labels known states and shows anything else as Unknown", () => {
    expect(stateLabel("downloading")).toBe("Downloading");
    expect(stateLabel("retry_waiting")).toBe("Waiting to retry");
    expect(stateLabel("something_new")).toBe("Unknown");
  });

  it("formats durations compactly", () => {
    expect(formatDuration(0)).toBe("0s");
    expect(formatDuration(4000)).toBe("4s");
    expect(formatDuration(125_000)).toBe("2m 05s");
    expect(formatDuration(3_725_000)).toBe("1h 02m");
    expect(formatDuration(90_000_000)).toBe("1d 1h");
  });

  it("eta_labels_estimating_unknown_or_a_duration", () => {
    expect(formatEta({ state: "downloading", total_bytes: 4096, eta_ms: 4000 })).toBe("4s");
    expect(formatEta({ state: "downloading", total_bytes: 4096 })).toBe("Estimating...");
    expect(formatEta({ state: "probing" })).toBe("Unknown");
    expect(formatEta({ state: "downloading" })).toBe("Unknown");
    // Never a made-up number for an inactive download.
    for (const state of ["paused", "completed", "failed", "queued", "teleporting"]) {
      expect(formatEta({ state, total_bytes: 10, eta_ms: 5 })).toBe("");
    }
  });
});
