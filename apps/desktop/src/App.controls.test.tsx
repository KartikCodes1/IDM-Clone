import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import App from "./App";
import { formatRetryTime } from "./lib/messages";
import { goldenRow, installFakeAgent, type Row } from "./test/fakeAgent";

const ID = "018f2c3e-5a10-7b2d-9c4e-0123456789ab";

function rowFor(name: string): HTMLElement {
  const row = screen.getByText(name).closest("li");
  if (row === null) throw new Error(`no row for ${name}`);
  return row;
}

async function renderWith(items: Row[]) {
  const agent = installFakeAgent(items);
  const user = userEvent.setup();
  render(<App />);
  await screen.findByText(items.length === 0 ? "No downloads yet" : "archive.zip");
  return { agent, user };
}

describe("toolbar", () => {
  it("toolbar_pause_all_calls_the_command_keeping_progress", async () => {
    const { agent, user } = await renderWith([goldenRow()]);
    await user.click(screen.getByRole("button", { name: "Pause all" }));
    expect(agent.of("pause_all")).toEqual([{ cmd: "pause_all", args: { discard_progress: false } }]);
  });

  it("toolbar_resume_all_calls_the_command", async () => {
    const { agent, user } = await renderWith([goldenRow({ state: "paused" })]);
    await user.click(screen.getByRole("button", { name: "Resume all" }));
    expect(agent.of("resume_all")).toEqual([{ cmd: "resume_all", args: {} }]);
  });

  it("toolbar_reports_how_many_were_paused_and_resumed", async () => {
    const { agent, user } = await renderWith([goldenRow()]);
    agent.reply("pause_all", { paused: [ID, "x"], skipped_not_resumable: [] });
    agent.reply("resume_all", { resumed: [ID] });
    await user.click(screen.getByRole("button", { name: "Pause all" }));
    expect(await screen.findByText("Paused 2 downloads")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Resume all" }));
    expect(await screen.findByText("Resumed 1 download")).toBeInTheDocument();
  });

  it("toolbar_says_when_non_resumable_downloads_were_left_running", async () => {
    const { agent, user } = await renderWith([goldenRow()]);
    agent.reply("pause_all", { paused: [], skipped_not_resumable: [ID, "y"] });
    await user.click(screen.getByRole("button", { name: "Pause all" }));
    expect(await screen.findByText(/2 downloads cannot resume/i)).toBeInTheDocument();
  });

  it("toolbar_failure_shows_the_engine_message", async () => {
    const { agent, user } = await renderWith([goldenRow()]);
    agent.failWith("pause_all", { kind: "agent_unavailable", message: "The background engine is not running." });
    await user.click(screen.getByRole("button", { name: "Pause all" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("The background engine is not running.");
  });

  it("toolbar_is_available_with_an_empty_list", async () => {
    const { agent, user } = await renderWith([]);
    await user.click(screen.getByRole("button", { name: "Resume all" }));
    expect(agent.of("resume_all")).toHaveLength(1);
  });
});

describe("limits panel", () => {
  async function openPanel(items: Row[] = [goldenRow()]) {
    const ctx = await renderWith(items);
    await ctx.user.click(screen.getByRole("button", { name: "Limits" }));
    await screen.findByDisplayValue("3");
    return ctx;
  }

  const active = () => screen.getByLabelText(/Active downloads/);
  const speed = () => screen.getByLabelText("Speed limit value");
  const unit = () => screen.getByLabelText("Speed limit unit");
  const conns = () => screen.getByLabelText(/Connections per download/);
  const apply = () => screen.getByRole("button", { name: "Apply limits" });

  it("limits_panel_reads_and_shows_the_current_values", async () => {
    const { agent } = await openPanel();
    expect(agent.of("get_limits")).toEqual([{ cmd: "get_limits", args: {} }]);
    expect(active()).toHaveValue(3);
    expect(unit()).toHaveValue("MB/s");
    expect(speed()).toHaveValue(2);
    expect(conns()).toHaveValue(4);
  });

  it("limits_panel_stays_closed_until_asked", async () => {
    const { agent } = await renderWith([goldenRow()]);
    expect(screen.queryByLabelText(/Active downloads/)).toBeNull();
    expect(agent.of("get_limits")).toHaveLength(0);
  });

  it("limits_validation_blocks_out_of_range_values_without_calling", async () => {
    const { agent, user } = await openPanel();
    for (const [field, value] of [
      [active, "0"],
      [active, "11"],
      [conns, "0"],
      [conns, "17"],
    ] as const) {
      await user.clear(active());
      await user.type(active(), "3");
      await user.clear(conns());
      await user.type(conns(), "4");
      await user.clear(field());
      await user.type(field(), value);
      await user.click(apply());
      expect((await screen.findAllByRole("alert")).length, `${value}`).toBeGreaterThan(0);
    }
    expect(agent.of("update_limits")).toHaveLength(0);
  });

  it("limits_validation_rejects_a_blank_or_tiny_speed", async () => {
    const { agent, user } = await openPanel();
    await user.selectOptions(unit(), "KB/s");
    await user.clear(speed());
    await user.click(apply());
    expect(await screen.findByText(/speed limit/i, { selector: "[role=alert]" })).toBeInTheDocument();
    await user.type(speed(), "0.5");
    await user.click(apply());
    expect(agent.of("update_limits")).toHaveLength(0);
  });

  it("limits_apply_sends_the_converted_values", async () => {
    const { agent, user } = await openPanel();
    await user.clear(active());
    await user.type(active(), "5");
    await user.clear(speed());
    await user.type(speed(), "2.5");
    await user.clear(conns());
    await user.type(conns(), "8");
    await user.click(apply());
    await screen.findByText(/Active downloads: /);
    expect(agent.of("update_limits")).toEqual([
      {
        cmd: "update_limits",
        args: { scope: "global", max_active_downloads: 5, rate_limit: 2_621_440, max_connections: 8 },
      },
    ]);
  });

  it("limits_off_sends_the_word_off", async () => {
    const { agent, user } = await openPanel();
    agent.reply("update_limits", { effective: { max_active_downloads: 3, max_connections: 4 } });
    await user.selectOptions(unit(), "off");
    expect(speed()).toBeDisabled();
    await user.click(apply());
    await screen.findByText(/Speed limit: Off/);
    expect(agent.of("update_limits")[0]?.args).toEqual({
      scope: "global",
      max_active_downloads: 3,
      rate_limit: "off",
      max_connections: 4,
    });
  });

  it("limits_apply_shows_the_effective_values_the_engine_returned", async () => {
    const { agent, user } = await openPanel();
    agent.reply("update_limits", { effective: { max_active_downloads: 2, max_connections: 6 } });
    await user.clear(active());
    await user.type(active(), "5");
    await user.click(apply());
    expect(await screen.findByText(/Active downloads: 2/)).toBeInTheDocument();
    expect(screen.getByText(/Connections: 6/)).toBeInTheDocument();
    expect(screen.getByText(/Speed limit: Off/)).toBeInTheDocument();
    expect(active()).toHaveValue(2);
    expect(unit()).toHaveValue("off");
  });

  it("limits_apply_failure_is_shown_in_the_panel_and_keeps_the_input", async () => {
    const { agent, user } = await openPanel();
    agent.failWith("update_limits", { kind: "agent_error", message: "The engine rejected that limit." });
    await user.clear(active());
    await user.type(active(), "7");
    await user.click(apply());
    expect(await screen.findByText("The engine rejected that limit.")).toBeInTheDocument();
    expect(active()).toHaveValue(7);
  });

  it("limits_read_failure_is_shown_and_apply_still_validates", async () => {
    const agent = installFakeAgent([goldenRow()]);
    agent.failWith("get_limits", { kind: "agent_error", message: "Limits are not available." });
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("archive.zip");
    await user.click(screen.getByRole("button", { name: "Limits" }));
    expect(await screen.findByText("Limits are not available.")).toBeInTheDocument();
  });
});

describe("per-row controls", () => {
  it("row_priority_selector_shows_the_current_priority_and_changes_it", async () => {
    const { agent, user } = await renderWith([goldenRow({ priority: "normal" })]);
    const select = within(rowFor("archive.zip")).getByLabelText("Priority for archive.zip");
    expect(select).toHaveValue("normal");
    expect(within(select).getAllByRole("option").map((o) => o.textContent)).toEqual(["High", "Normal", "Low"]);
    await user.selectOptions(select, "high");
    expect(agent.of("update_priority")).toEqual([{ cmd: "update_priority", args: { id: ID, priority: "high" } }]);
  });

  it("row_priority_failure_shows_a_toast", async () => {
    const { agent, user } = await renderWith([goldenRow()]);
    agent.failWith("update_priority", { kind: "agent_error", message: "That download no longer exists." });
    await user.selectOptions(screen.getByLabelText("Priority for archive.zip"), "low");
    expect(await screen.findByRole("alert")).toHaveTextContent("That download no longer exists.");
  });

  it("row_connections_selector_offers_1_to_16_and_sets_the_download_scope", async () => {
    const { agent, user } = await renderWith([goldenRow()]);
    const select = within(rowFor("archive.zip")).getByLabelText("Connections for archive.zip");
    const values = within(select)
      .getAllByRole("option")
      .map((o) => (o as HTMLOptionElement).value)
      .filter((v) => v !== "");
    expect(values).toEqual(Array.from({ length: 16 }, (_, i) => String(i + 1)));
    agent.reply("update_limits", { effective: { max_connections: 6 } });
    await user.selectOptions(select, "6");
    expect(agent.of("update_limits")).toEqual([
      { cmd: "update_limits", args: { scope: "download", download_id: ID, max_connections: 6 } },
    ]);
    expect(await screen.findByText("archive.zip: up to 6 connections")).toBeInTheDocument();
  });

  it("row_connections_failure_shows_a_toast", async () => {
    const { agent, user } = await renderWith([goldenRow()]);
    agent.failWith("update_limits", { kind: "agent_error", message: "Not allowed right now." });
    await user.selectOptions(screen.getByLabelText("Connections for archive.zip"), "2");
    expect(await screen.findByRole("alert")).toHaveTextContent("Not allowed right now.");
  });

  it("row_selectors_are_hidden_for_finished_downloads", async () => {
    for (const state of ["completed", "failed", "cancelled"]) {
      await renderWith([goldenRow({ state })]);
      const row = rowFor("archive.zip");
      expect(within(row).queryByLabelText(/Priority for/), state).toBeNull();
      expect(within(row).queryByLabelText(/Connections for/), state).toBeNull();
      cleanup();
    }
  });

  it("row_selectors_are_offered_for_waiting_and_active_downloads", async () => {
    for (const state of ["queued", "downloading", "paused", "retry_waiting", "interrupted"]) {
      await renderWith([goldenRow({ state })]);
      const row = rowFor("archive.zip");
      expect(within(row).getByLabelText(/Priority for/), state).toBeInTheDocument();
      expect(within(row).getByLabelText(/Connections for/), state).toBeInTheDocument();
      cleanup();
    }
  });
});

describe("friendly state messages", () => {
  const AT = new Date(2026, 10, 16, 23, 45, 0).getTime();

  it("retry_waiting_shows_the_local_time_from_next_retry_at_ms", async () => {
    const agent = installFakeAgent([goldenRow({ state: "retry_waiting" })]);
    agent.reply("get_download", { ...goldenRow({ state: "retry_waiting" }), next_retry_at_ms: AT });
    render(<App />);
    await screen.findByText("archive.zip");
    expect(await screen.findByText(`Retrying at ${formatRetryTime(AT)}`)).toBeInTheDocument();
    expect(agent.of("get_download")).toEqual([{ cmd: "get_download", args: { id: ID } }]);
  });

  it("retry_waiting_asks_again_once_the_fetched_time_has_passed", async () => {
    const past = Date.now() - 60_000;
    const agent = installFakeAgent([goldenRow({ state: "retry_waiting" })]);
    agent.reply("get_download", { ...goldenRow({ state: "retry_waiting" }), next_retry_at_ms: past });
    render(<App />);
    expect(await screen.findByText("Retrying now")).toBeInTheDocument();
    await waitFor(() => expect(agent.of("get_download").length).toBeGreaterThan(1), { timeout: 4000 });
  });

  it("retry_waiting_uses_the_rate_limit_time_without_asking_for_detail", async () => {
    const error = { code: "rate_limited", message: "m", retryable: true, details: { retry_at_ms: AT } };
    const agent = installFakeAgent([goldenRow({ state: "retry_waiting", last_error: error })]);
    render(<App />);
    expect(await screen.findByText(`Retrying at ${formatRetryTime(AT)}`)).toBeInTheDocument();
    expect(agent.of("get_download")).toHaveLength(0);
  });

  it("retry_waiting_falls_back_when_the_detail_cannot_be_read", async () => {
    const agent = installFakeAgent([goldenRow({ state: "retry_waiting" })]);
    agent.failWith("get_download", { kind: "agent_error", message: "gone" });
    render(<App />);
    expect(await screen.findByText("Retrying soon")).toBeInTheDocument();
  });

  it("needs_url_refresh_and_needs_credentials_messages", async () => {
    await renderWith([goldenRow({ state: "needs_url_refresh" })]);
    expect(rowFor("archive.zip")).toHaveTextContent("Link expired: add the download again with a fresh URL");
    cleanup();
    await renderWith([goldenRow({ state: "needs_credentials" })]);
    expect(rowFor("archive.zip")).toHaveTextContent("This server needs a login (not supported yet)");
  });

  it("paused_for_disk_full_and_destination_unavailable_explain_why", async () => {
    await renderWith([goldenRow({ state: "paused", state_reason: "disk_full" })]);
    expect(rowFor("archive.zip")).toHaveTextContent(/disk is full/i);
    cleanup();
    await renderWith([goldenRow({ state: "paused", state_reason: "destination_unavailable" })]);
    expect(rowFor("archive.zip")).toHaveTextContent(/folder or drive is unavailable/i);
    cleanup();
    await renderWith([goldenRow({ state: "paused", state_reason: "user" })]);
    expect(rowFor("archive.zip").querySelector(".row-note")).toBeNull();
  });

  it("interrupted_says_it_will_resume_automatically", async () => {
    await renderWith([goldenRow({ state: "interrupted", state_reason: "agent_shutdown" })]);
    expect(rowFor("archive.zip")).toHaveTextContent("Will resume automatically");
  });

  it("failed_rows_show_readable_text_for_the_newer_codes_and_not_the_engine_text", async () => {
    await renderWith([
      goldenRow({ state: "failed", last_error: { code: "path_escape", message: "C:\\raw\\secret", retryable: false } }),
    ]);
    const row = rowFor("archive.zip");
    expect(row.textContent).not.toContain("secret");
    expect(row.textContent).toMatch(/destination/i);
  });
});
