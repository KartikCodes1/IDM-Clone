import { describe, expect, it } from "vitest";

import { describeLimits, formFromEffective, parseLimitsResult, validateLimitsForm, type LimitsForm } from "./limits";

const OK: LimitsForm = { maxActive: "3", speedUnit: "off", speedValue: "", connections: "4" };

describe("validateLimitsForm", () => {
  it("accepts_the_edges_of_every_range", () => {
    for (const [maxActive, connections] of [
      ["1", "1"],
      ["10", "16"],
    ] as const) {
      const result = validateLimitsForm({ ...OK, maxActive, connections });
      expect(result.ok, `${maxActive}/${connections}`).toBe(true);
    }
  });

  it("rejects_values_outside_the_ranges_and_non_integers", () => {
    const bad: Partial<LimitsForm>[] = [
      { maxActive: "0" },
      { maxActive: "11" },
      { maxActive: "" },
      { maxActive: "2.5" },
      { maxActive: "abc" },
      { connections: "0" },
      { connections: "17" },
      { connections: "-1" },
      { connections: "1e1" },
    ];
    for (const patch of bad) {
      const result = validateLimitsForm({ ...OK, ...patch });
      expect(result.ok, JSON.stringify(patch)).toBe(false);
    }
  });

  it("reports_each_field_error_separately", () => {
    const result = validateLimitsForm({ maxActive: "0", speedUnit: "KB/s", speedValue: "", connections: "99" });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(Object.keys(result.errors).sort()).toEqual(["connections", "maxActive", "speed"]);
  });

  it("converts_kb_and_mb_to_bytes_per_second_and_off_to_off", () => {
    const kb = validateLimitsForm({ ...OK, speedUnit: "KB/s", speedValue: "512" });
    expect(kb.ok && kb.value.rate_limit).toBe(512 * 1024);
    const mb = validateLimitsForm({ ...OK, speedUnit: "MB/s", speedValue: "2.5" });
    expect(mb.ok && mb.value.rate_limit).toBe(2_621_440);
    const off = validateLimitsForm({ ...OK, speedUnit: "off", speedValue: "garbage ignored" });
    expect(off.ok && off.value.rate_limit).toBe("off");
  });

  it("rejects_a_speed_below_1_kb_per_second_or_not_a_number", () => {
    for (const speedValue of ["", "0", "0.5", "-3", "fast", "Infinity", "1e400"]) {
      expect(validateLimitsForm({ ...OK, speedUnit: "KB/s", speedValue }).ok, speedValue).toBe(false);
    }
    expect(validateLimitsForm({ ...OK, speedUnit: "KB/s", speedValue: "1" }).ok).toBe(true);
  });

  it("rejects_a_speed_too_large_for_a_safe_integer", () => {
    expect(validateLimitsForm({ ...OK, speedUnit: "MB/s", speedValue: "99999999999999" }).ok).toBe(false);
  });
});

describe("effective limits", () => {
  it("parses_the_result_and_treats_a_missing_rate_as_unlimited", () => {
    expect(parseLimitsResult({ effective: { max_connections: 4, max_active_downloads: 3 } })).toEqual({
      max_connections: 4,
      max_active_downloads: 3,
    });
    expect(parseLimitsResult({ effective: { rate_limit_bps: 4096 } }).rate_limit_bps).toBe(4096);
  });

  it("rejects_a_malformed_result", () => {
    expect(() => parseLimitsResult({})).toThrow();
    expect(() => parseLimitsResult({ effective: { max_connections: "4" } })).toThrow();
    expect(() => parseLimitsResult({ effective: { max_connections: -1 } })).toThrow();
  });

  it("fills_the_form_choosing_the_largest_exact_unit", () => {
    expect(formFromEffective({ max_connections: 4, max_active_downloads: 3 })).toEqual({
      maxActive: "3",
      speedUnit: "off",
      speedValue: "",
      connections: "4",
    });
    expect(formFromEffective({ rate_limit_bps: 2_097_152 })).toMatchObject({ speedUnit: "MB/s", speedValue: "2" });
    expect(formFromEffective({ rate_limit_bps: 1536 })).toMatchObject({ speedUnit: "KB/s", speedValue: "1.5" });
  });

  it("describes_the_values_in_force", () => {
    const text = describeLimits({ max_connections: 4, max_active_downloads: 3, rate_limit_bps: 2_097_152 });
    expect(text).toContain("Active downloads: 3");
    expect(text).toContain("Speed limit: 2.0 MiB/s");
    expect(text).toContain("Connections: 4");
    expect(describeLimits({})).toContain("Speed limit: Off");
  });
});
