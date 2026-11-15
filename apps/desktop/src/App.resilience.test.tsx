import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import App from "./App";
import { goldenRow, installFakeAgent } from "./test/fakeAgent";

afterEach(cleanup);

describe("transient poll failures", () => {
  it("a_failed_poll_keeps_the_form_and_list_and_shows_a_banner", async () => {
    const agent = installFakeAgent([goldenRow()]);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("archive.zip");
    const url = screen.getByLabelText("Download link");
    await user.type(url, "https://example.com/half-typed");

    agent.failWith("get_snapshot", { kind: "internal", message: "Communication with the background engine failed." });
    await screen.findByText("Engine request failed", undefined, { timeout: 3000 });

    expect(screen.getByLabelText("Download link")).toHaveValue("https://example.com/half-typed");
    expect(screen.getByText("archive.zip")).toBeInTheDocument();
  });
});

describe("focus management", () => {
  it("remove_confirmation_moves_focus_and_restores_it", async () => {
    installFakeAgent([goldenRow()]);
    const user = userEvent.setup();
    render(<App />);
    const row = (await screen.findByText("archive.zip")).closest("li") as HTMLElement;
    await user.click(within(row).getByRole("button", { name: /Remove archive.zip/ }));
    const group = within(row).getByRole("group", { name: "Confirm removal" });
    expect(within(group).getAllByRole("button")[0]).toHaveFocus();
    await user.click(within(group).getByRole("button", { name: "Keep download" }));
    expect(within(row).getByRole("button", { name: /Remove archive.zip/ })).toHaveFocus();
  });

  it("stop_engine_confirmation_moves_focus_and_restores_it", async () => {
    installFakeAgent([goldenRow()]);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("archive.zip");
    await user.click(screen.getByRole("button", { name: "Stop engine" }));
    expect(screen.getByRole("button", { name: "Stop now" })).toHaveFocus();
    await user.click(screen.getByRole("button", { name: "Keep running" }));
    expect(screen.getByRole("button", { name: "Stop engine" })).toHaveFocus();
  });
});
