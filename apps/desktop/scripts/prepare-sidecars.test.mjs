import { describe, expect, it } from "vitest";

import { TARGET_TRIPLE, sidecarPlan } from "./prepare-sidecars.mjs";

describe("sidecarPlan", () => {
  const plan = sidecarPlan({ repoRoot: String.raw`C:\repo`, targetDir: String.raw`C:\repo\target` });

  it("builds only the agent and the cli, release, locked", () => {
    expect(plan.cargoArgs).toEqual(["build", "--release", "--locked", "-p", "forgedm-agent", "-p", "forgedm-cli"]);
  });

  it("copies both exes to the target-triple names Tauri expects", () => {
    expect(TARGET_TRIPLE).toBe("x86_64-pc-windows-msvc");
    expect(plan.copies.map((c) => c.to.split(/[\\/]/).slice(-1)[0])).toEqual([
      "forgedm-agent-x86_64-pc-windows-msvc.exe",
      "forgedm-x86_64-pc-windows-msvc.exe",
    ]);
    expect(plan.copies[0].from).toMatch(/release[\\/]forgedm-agent\.exe$/);
    expect(plan.copies[1].from).toMatch(/release[\\/]forgedm\.exe$/);
    for (const c of plan.copies) expect(c.to).toMatch(/src-tauri[\\/]binaries[\\/]/);
  });
});
