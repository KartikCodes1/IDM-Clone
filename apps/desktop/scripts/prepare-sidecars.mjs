// Builds release forgedm-agent and forgedm (the CLI) and stages them where Tauri's
// `externalBin` expects them: src-tauri/binaries/<name>-<target triple>.exe. The staged
// files are generated and git-ignored. Run by the installer overlay's beforeBuildCommand
// (src-tauri/tauri.bundle.conf.json); never by plain `cargo test` or `tauri build --no-bundle`.
//
// Tauri installs a sidecar next to the app exe under its plain name (the triple is stripped),
// which is where the desktop looks for forgedm-agent.exe.

import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const TARGET_TRIPLE = "x86_64-pc-windows-msvc";
const BINARIES = [
  { crate: "forgedm-agent", exe: "forgedm-agent.exe", sidecar: "forgedm-agent" },
  { crate: "forgedm-cli", exe: "forgedm.exe", sidecar: "forgedm" },
];

/** Pure description of the work: the cargo arguments and the file copies. */
export function sidecarPlan({ repoRoot, targetDir }) {
  const releaseDir = path.win32.join(targetDir, "release");
  const stageDir = path.win32.join(repoRoot, "apps", "desktop", "src-tauri", "binaries");
  return {
    cargoArgs: ["build", "--release", "--locked", ...BINARIES.flatMap((b) => ["-p", b.crate])],
    stageDir,
    copies: BINARIES.map((b) => ({
      from: path.win32.join(releaseDir, b.exe),
      to: path.win32.join(stageDir, `${b.sidecar}-${TARGET_TRIPLE}.exe`),
    })),
  };
}

function main() {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const repoRoot = path.resolve(here, "..", "..", "..");
  const targetDir = process.env.CARGO_TARGET_DIR ?? path.join(repoRoot, "target");
  const plan = sidecarPlan({ repoRoot, targetDir });

  const built = spawnSync("cargo", plan.cargoArgs, { cwd: repoRoot, stdio: "inherit" });
  if (built.status !== 0) {
    process.stderr.write("prepare-sidecars: cargo build failed\n");
    process.exit(built.status ?? 1);
  }
  mkdirSync(plan.stageDir, { recursive: true });
  for (const { from, to } of plan.copies) {
    copyFileSync(from, to);
    process.stdout.write(`prepare-sidecars: ${path.basename(to)}\n`);
  }
}

if (process.argv[1] !== undefined && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
