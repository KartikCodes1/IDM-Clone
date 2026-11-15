import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import App from "./App";
import { AGENT_STATUS, goldenRow, installFakeAgent, type Row } from "./test/fakeAgent";

const ID = "018f2c3e-5a10-7b2d-9c4e-0123456789ab";
const SHA = "0123456789abcdef".repeat(4);

function rowFor(name: string): HTMLElement {
  const row = screen.getByText(name).closest("li");
  if (row === null) throw new Error(`no row for ${name}`);
  return row;
}

function buttonNames(row: HTMLElement): string[] {
  return within(row)
    .queryAllByRole("button")
    .map((b) => (b.textContent ?? "").trim());
}

async function renderWith(items: Row[]) {
  const agent = installFakeAgent(items);
  const user = userEvent.setup();
  render(<App />);
  await screen.findByText("archive.zip");
  return { agent, user };
}

describe("download table", () => {
  it("table_renders_true_values", async () => {
    await renderWith([goldenRow()]);
    const row = rowFor("archive.zip");
    expect(within(row).getByText("example.com")).toBeInTheDocument();
    expect(within(row).getByText("Downloading")).toBeInTheDocument();
    expect(within(row).getByText("25%")).toBeInTheDocument();
    expect(row.textContent).toContain("1.0 KiB of 4.0 KiB");
    expect(row.textContent).toContain("512 B/s");
    expect(within(row).getByText("ETA 4s")).toBeInTheDocument();
    expect(within(row).getByRole("progressbar")).toHaveAttribute("aria-valuenow", "25");
  });

  it("unknown_total_indeterminate", async () => {
    const row = goldenRow();
    delete row["total_bytes"];
    delete row["eta_ms"];
    await renderWith([row]);
    const bar = within(rowFor("archive.zip")).getByRole("progressbar");
    expect(bar).not.toHaveAttribute("aria-valuenow");
    expect(bar.className).toContain("progress-indeterminate");
  });

  it("eta_unknown_label", async () => {
    const unknownTotal = goldenRow();
    delete unknownTotal["total_bytes"];
    delete unknownTotal["eta_ms"];
    const estimating = goldenRow({ id: "018f2c3e-5a10-7b2d-9c4e-0123456789ad", display_name: "other.iso" });
    delete estimating["eta_ms"];
    await renderWith([unknownTotal, estimating]);
    expect(within(rowFor("archive.zip")).getByText("ETA Unknown")).toBeInTheDocument();
    expect(within(rowFor("other.iso")).getByText("ETA Estimating...")).toBeInTheDocument();
  });

  it("shows_a_friendly_reason_for_a_failed_download", async () => {
    await renderWith([
      goldenRow({ state: "failed", last_error: { code: "disk_full", message: "raw agent text", retryable: true } }),
    ]);
    const row = rowFor("archive.zip");
    expect(row.textContent).toMatch(/disk is full/i);
    expect(row.textContent).not.toContain("raw agent text");
  });
});

describe("row buttons", () => {
  const cases: [string, string[]][] = [
    ["queued", ["Pause", "Cancel", "Remove"]],
    ["downloading", ["Pause", "Cancel", "Remove"]],
    ["paused", ["Resume", "Cancel", "Remove"]],
    ["failed", ["Retry", "Remove"]],
    ["cancelled", ["Retry", "Remove"]],
    ["completed", ["Show in folder", "Remove"]],
    ["finalizing", []],
  ];
  for (const [state, expected] of cases) {
    it(`buttons_for_${state}_state`, async () => {
      await renderWith([goldenRow({ state })]);
      expect(buttonNames(rowFor("archive.zip"))).toEqual(expected);
    });
  }

  it("pause_resume_cancel_retry_call_the_right_command", async () => {
    const table: [string, string, string, Record<string, unknown>][] = [
      ["downloading", "Pause", "pause", { id: ID, discard_progress: false }],
      ["paused", "Resume", "resume", { id: ID }],
      ["downloading", "Cancel", "cancel", { id: ID }],
      ["failed", "Retry", "retry", { id: ID, restart_from_zero: false }],
      ["completed", "Show in folder", "show_in_folder", { id: ID }],
    ];
    for (const [state, label, cmd, args] of table) {
      const { agent, user } = await renderWith([goldenRow({ state })]);
      await user.click(within(rowFor("archive.zip")).getByRole("button", { name: new RegExp(`^${label}`) }));
      expect(agent.of(cmd), `${state}/${label}`).toEqual([{ cmd, args }]);
      screen.getByText("archive.zip");
      cleanup();
    }
  });

  it("retry_restarts_from_zero_after_a_checksum_mismatch", async () => {
    const { agent, user } = await renderWith([
      goldenRow({ state: "failed", last_error: { code: "checksum_mismatch", message: "m", retryable: false } }),
    ]);
    await user.click(within(rowFor("archive.zip")).getByRole("button", { name: /^Retry/ }));
    expect(agent.of("retry")[0]?.args).toEqual({ id: ID, restart_from_zero: true });
  });

  it("remove_needs_an_explicit_confirm_and_delete_partial_is_a_separate_choice", async () => {
    const { agent, user } = await renderWith([goldenRow({ state: "paused" })]);
    const row = () => rowFor("archive.zip");

    await user.click(within(row()).getByRole("button", { name: /^Remove/ }));
    expect(agent.of("remove")).toHaveLength(0);

    await user.click(within(row()).getByRole("button", { name: "Keep download" }));
    expect(agent.of("remove")).toHaveLength(0);
    expect(within(row()).queryByRole("button", { name: /delete partial/i })).toBeNull();

    await user.click(within(row()).getByRole("button", { name: /^Remove/ }));
    await user.click(within(row()).getByRole("button", { name: "Remove from list" }));
    expect(agent.of("remove")).toEqual([{ cmd: "remove", args: { id: ID, delete_partial: false } }]);

    await user.click(within(row()).getByRole("button", { name: /^Remove/ }));
    await user.click(within(row()).getByRole("button", { name: "Remove and delete partial file" }));
    expect(agent.of("remove")[1]?.args).toEqual({ id: ID, delete_partial: true });
  });

  it("completed_downloads_never_offer_to_delete_a_partial_file", async () => {
    const { user } = await renderWith([goldenRow({ state: "completed", final_path: String.raw`C:\Dl\archive.zip` })]);
    await user.click(within(rowFor("archive.zip")).getByRole("button", { name: /^Remove/ }));
    expect(screen.queryByRole("button", { name: /delete partial/i })).toBeNull();
    expect(screen.getByRole("button", { name: "Remove from list" })).toBeInTheDocument();
  });

  it("a_failed_command_shows_the_friendly_message_in_a_toast", async () => {
    const { agent, user } = await renderWith([goldenRow()]);
    agent.failWith("pause", { kind: "agent_error", message: "That download no longer exists." });
    await user.click(within(rowFor("archive.zip")).getByRole("button", { name: /^Pause/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent("That download no longer exists.");
  });

  it("refreshes_the_snapshot_right_after_a_command", async () => {
    const { agent, user } = await renderWith([goldenRow()]);
    const before = agent.of("get_snapshot").length;
    await user.click(within(rowFor("archive.zip")).getByRole("button", { name: /^Pause/ }));
    await screen.findByText("archive.zip");
    expect(agent.of("get_snapshot").length).toBeGreaterThan(before);
  });
});

describe("add download panel", () => {
  async function setup() {
    const agent = installFakeAgent([]);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("No downloads yet");
    return { agent, user };
  }

  it("add_form_rejects_non_http_urls_without_calling_the_engine", async () => {
    const { agent, user } = await setup();
    await user.type(screen.getByLabelText("Download link"), "ftp://example.com/a.zip");
    await user.click(screen.getByRole("button", { name: "Add" }));
    expect(await screen.findByText(/http or https/i)).toBeInTheDocument();
    expect(agent.of("enqueue_url")).toHaveLength(0);
  });

  it("add_form_rejects_a_bad_checksum_and_a_relative_folder", async () => {
    const { agent, user } = await setup();
    await user.type(screen.getByLabelText("Download link"), "https://example.com/a.zip");
    await user.type(screen.getByLabelText(/Save to folder/), "Downloads");
    await user.type(screen.getByLabelText(/SHA-256/), "abc");
    await user.click(screen.getByRole("button", { name: "Add" }));
    expect(await screen.findByText(/64 hexadecimal/i)).toBeInTheDocument();
    expect(screen.getByText(/full folder path/i)).toBeInTheDocument();
    expect(agent.of("enqueue_url")).toHaveLength(0);
  });

  it("add_form_submits_the_trimmed_values_and_resets", async () => {
    const { agent, user } = await setup();
    await user.type(screen.getByLabelText("Download link"), "  https://example.com/a.zip  ");
    await user.type(screen.getByLabelText(/Save to folder/), String.raw`D:\Files`);
    await user.type(screen.getByLabelText(/SHA-256/), SHA);
    await user.click(screen.getByRole("button", { name: "Add" }));
    expect(await screen.findByText(/Added archive\.zip/)).toBeInTheDocument();
    expect(agent.of("enqueue_url")).toEqual([
      { cmd: "enqueue_url", args: { url: "https://example.com/a.zip", target_dir: String.raw`D:\Files`, sha256: SHA } },
    ]);
    expect(screen.getByLabelText("Download link")).toHaveValue("");
  });

  it("add_form_submits_with_only_a_url_when_the_other_fields_are_empty", async () => {
    const { agent, user } = await setup();
    await user.type(screen.getByLabelText("Download link"), "http://example.com/b.bin{Enter}");
    await screen.findByText(/Added archive\.zip/);
    expect(agent.of("enqueue_url")[0]?.args).toEqual({ url: "http://example.com/b.bin" });
  });

  it("add_form_shows_the_engine_error_inline_and_keeps_the_input", async () => {
    const { agent, user } = await setup();
    agent.failWith("enqueue_url", { kind: "agent_error", message: "The link was rejected. Check that it is a valid http or https address." });
    await user.type(screen.getByLabelText("Download link"), "https://example.com/a.zip");
    await user.click(screen.getByRole("button", { name: "Add" }));
    expect(await screen.findByText(/link was rejected/i)).toBeInTheDocument();
    expect(screen.getByLabelText("Download link")).toHaveValue("https://example.com/a.zip");
  });

  it("never_puts_the_typed_url_in_a_toast", async () => {
    const { user } = await setup();
    await user.type(screen.getByLabelText("Download link"), "https://user:secret@example.com/a.zip?token=abc");
    await user.click(screen.getByRole("button", { name: "Add" }));
    await screen.findByText(/Added archive\.zip/);
    expect(document.body.textContent).not.toContain("secret");
    expect(document.body.textContent).not.toContain("token=abc");
  });
});

describe("engine controls", () => {
  it("engine_banner_and_start_button_when_the_engine_is_not_running", async () => {
    const agent = installFakeAgent([]);
    agent.failWith("get_snapshot", { kind: "agent_unavailable", message: "The background engine is not running." });
    agent.failWith("start_engine", { kind: "agent_unavailable", message: "The background engine could not be started." });
    const user = userEvent.setup();
    render(<App />);
    expect(await screen.findByText("Background engine not running")).toBeInTheDocument();
    // Auto-start was attempted once at launch and its failure is shown.
    expect(await screen.findByText("The background engine could not be started.")).toBeInTheDocument();
    expect(agent.of("start_engine")).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: "Start engine" }));
    expect(agent.of("start_engine")).toHaveLength(2);
  });

  it("auto_starts_the_engine_once_and_recovers", async () => {
    const agent = installFakeAgent([goldenRow()]);
    agent.failWith("get_snapshot", { kind: "agent_unavailable", message: "The background engine is not running." });
    render(<App />);
    await screen.findByText("Background engine not running");
    expect(agent.of("start_engine")).toHaveLength(1);
    // The engine comes up: the next poll succeeds and the banner goes away.
    installFakeAgent([goldenRow()]);
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Check again" }));
    expect(await screen.findByText("archive.zip")).toBeInTheDocument();
    expect(screen.queryByText("Background engine not running")).toBeNull();
  });

  it("does_not_start_the_engine_when_it_is_already_running", async () => {
    const agent = installFakeAgent([]);
    render(<App />);
    await screen.findByText("Engine running");
    expect(agent.of("start_engine")).toHaveLength(0);
  });

  it("stop_engine_asks_first_then_stops", async () => {
    const agent = installFakeAgent([]);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("Engine running");
    await user.click(screen.getByRole("button", { name: "Stop engine" }));
    expect(agent.of("stop_engine")).toHaveLength(0);
    expect(screen.getByText(/Downloads pause/i)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Stop now" }));
    expect(agent.of("stop_engine")).toHaveLength(1);
  });

  it("shows_the_engine_version_from_the_snapshot", async () => {
    installFakeAgent([]);
    render(<App />);
    expect(await screen.findByText(String(AGENT_STATUS["agent_version"]))).toBeInTheDocument();
  });
});
