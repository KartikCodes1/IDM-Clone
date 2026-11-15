import { mockIPC } from "@tauri-apps/api/mocks";
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import downloadListGolden from "../../../crates/forgedm-protocol/tests/golden/v1/response_ListDownloads.json";
import statusGolden from "../../../crates/forgedm-protocol/tests/golden/v1/response_GetAgentStatus.json";
import App from "./App";

type Row = Record<string, unknown>;

function mockAgent(items: Row[]): void {
  mockIPC((cmd) => {
    if (cmd === "agent_status") return statusGolden.result;
    if (cmd === "list_downloads") return { items, total: items.length };
    throw new Error(`unexpected command ${cmd}`);
  });
}

function goldenRow(): Row {
  const [row] = downloadListGolden.result.items as Row[];
  return { ...row };
}

describe("App", () => {
  it("App_shows_engine_not_running", async () => {
    mockIPC(() => {
      // Tauri rejects an invoke with the command error object itself.
      throw { kind: "agent_unavailable", message: "The background engine is not running." };
    });
    render(<App />);
    expect(await screen.findByText("Background engine not running")).toBeInTheDocument();
    expect(screen.queryByText("No downloads yet")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Check again" })).toBeEnabled();
  });

  it("App_shows_empty_state", async () => {
    mockAgent([]);
    render(<App />);
    expect(await screen.findByText("No downloads yet")).toBeInTheDocument();
    expect(await screen.findByText("Engine running")).toBeInTheDocument();
    expect(screen.getByText("0.1.0")).toBeInTheDocument();
    expect(screen.queryByText("Background engine not running")).not.toBeInTheDocument();
  });

  it("App_unknown_size_shows_unknown_not_percent", async () => {
    const row = goldenRow();
    delete row.total_bytes;
    delete row.eta_ms;
    mockAgent([row]);
    render(<App />);
    expect(await screen.findByText("archive.zip")).toBeInTheDocument();
    expect(screen.getByText("Unknown size")).toBeInTheDocument();
    expect(document.body.textContent).not.toMatch(/%/);
    const bar = screen.getByRole("progressbar");
    expect(bar).not.toHaveAttribute("aria-valuenow");
  });

  it("known_size_shows_a_real_percentage", async () => {
    mockAgent([goldenRow()]);
    render(<App />);
    expect(await screen.findByText("archive.zip")).toBeInTheDocument();
    expect(screen.getByText("25%")).toBeInTheDocument();
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "25");
  });

  it("malicious_name_renders_as_text", async () => {
    const payload = `<img src=x onerror="alert(1)"><script>alert(2)</script>`;
    mockAgent([{ ...goldenRow(), display_name: payload, url_display: payload }]);
    const { container } = render(<App />);
    expect((await screen.findAllByText(payload)).length).toBeGreaterThan(0);
    // The only <img> is the app's own brand mark, never the injected one.
    expect(container.querySelector('img[src="x"]')).toBeNull();
    expect(container.querySelectorAll("img")).toHaveLength(1);
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("[onerror]")).toBeNull();
  });

  it("malformed_payload_shows_an_error_not_a_crash", async () => {
    mockIPC((cmd) => (cmd === "agent_status" ? { nonsense: true } : { items: [], total: 0 }));
    render(<App />);
    expect(await screen.findByText("Unexpected answer from the engine")).toBeInTheDocument();
  });

  it("unknown_state_is_labelled_unknown", async () => {
    mockAgent([{ ...goldenRow(), state: "teleporting" }]);
    render(<App />);
    expect(await screen.findByText("Unknown")).toBeInTheDocument();
  });
});
