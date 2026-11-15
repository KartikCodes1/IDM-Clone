import { describe, expect, it } from "vitest";

import { formatBytes, formatSpeed, percentComplete, stateLabel } from "./format";

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
});
