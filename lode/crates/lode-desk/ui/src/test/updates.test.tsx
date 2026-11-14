import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setBackend, type UpdateCheck } from "../api";
import { App } from "../App";
import { DAY_MS } from "../components/Updates";
import { createMockBackend, type MockOptions } from "../mock";

const NEW: UpdateCheck = {
  state: "available",
  current: "0.1.0",
  version: "0.2.0",
  notes: "Branches, merges and lode:// links.",
  date: "2026-11-20T10:00:00Z",
};

function start(opts: MockOptions = {}) {
  const b = createMockBackend({ stepMs: 0, ...opts });
  setBackend(b);
  const user = userEvent.setup();
  render(<App />);
  return { b, user };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("updates", () => {
  it("offers a new version with its notes, and Later hides it", async () => {
    const { b, user } = start({ update: NEW });
    const check = vi.spyOn(b, "updateCheck");
    const banner = await screen.findByRole("region", { name: "Update" });
    expect(banner).toHaveTextContent("Lode Desk 0.2.0 is available.");
    expect(banner).toHaveTextContent("You have 0.1.0.");
    await user.click(within(banner).getByText("What's new"));
    expect(within(banner).getByText("Branches, merges and lode:// links.")).toBeVisible();
    await user.click(within(banner).getByRole("button", { name: "Later" }));
    expect(screen.queryByRole("region", { name: "Update" })).toBeNull();
    expect(check).not.toHaveBeenCalledWith(true);
  });

  it("installs and restarts, and says why when the package is refused", async () => {
    const refused = { code: "bad_signature", message: "the update is not signed with the Lode Desk update key" };
    const { b, user } = start({ update: NEW, refuseUpdate: refused });
    const install = vi.spyOn(b, "updateInstall");
    const banner = await screen.findByRole("region", { name: "Update" });
    await user.click(within(banner).getByRole("button", { name: "Install and restart" }));
    expect(install).toHaveBeenCalledTimes(1);
    expect(await within(banner).findByRole("alert")).toHaveTextContent("not signed with the Lode Desk update key");
  });

  it("checks again once a day", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const { b } = start();
    const check = vi.spyOn(b, "updateCheck");
    await screen.findByText("Recent workspaces");
    const before = check.mock.calls.length;
    await act(async () => {
      vi.advanceTimersByTime(DAY_MS);
    });
    await waitFor(() => expect(check.mock.calls.length).toBe(before + 1));
    expect(check).toHaveBeenLastCalledWith(false);
  });

  it("turns automatic checks off from Home and checks when asked", async () => {
    const { b, user } = start();
    const save = vi.spyOn(b, "setSettings");
    const auto = await screen.findByRole("checkbox", { name: "Check for updates automatically" });
    await waitFor(() => expect(auto).toBeEnabled());
    expect(auto).toBeChecked();
    await user.click(auto);
    expect(save).toHaveBeenCalledWith({ check_updates: false });
    expect(await b.updateCheck(false)).toEqual({
      state: "off",
      reason: "checking for updates is turned off in the settings",
    });
    await user.click(screen.getByRole("button", { name: "Check for updates now" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Lode Desk 0.1.0 is the newest version.");
  });

  it("shows an update found by Check now", async () => {
    const { b, user } = start();
    await screen.findByText("Recent workspaces");
    vi.spyOn(b, "updateCheck").mockResolvedValue(NEW);
    await user.click(screen.getByRole("button", { name: "Check for updates now" }));
    expect(await screen.findByRole("region", { name: "Update" })).toHaveTextContent("0.2.0");
  });
});
