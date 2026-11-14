import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { setBackend } from "../api";
import { App } from "../App";
import { createMockBackend, type MockOptions } from "../mock";

const WS = "D:\\Work\\Skyfall";

async function openSkyfall(opts: MockOptions = {}) {
  const b = createMockBackend({ stepMs: 0, ...opts });
  setBackend(b);
  const user = userEvent.setup();
  render(<App />);
  await user.click(await screen.findByText(WS));
  const pending = await screen.findByRole("region", { name: "Pending changes" });
  return { b, user, pending };
}

describe("locks", () => {
  it("locks the picked files with a comment and shows them as mine", async () => {
    const { b, user, pending } = await openSkyfall();
    const lock = vi.spyOn(b, "lock");
    await user.click(within(pending).getByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" }));
    await user.click(within(pending).getByRole("checkbox", { name: "Pick Source/Skyfall/Sail.cpp" }));
    // Sail.cpp is added, not in the depot yet: only Boat.cpp can be locked.
    await user.pointer({ keys: "[MouseRight]", target: within(pending).getByText("Boat.cpp") });
    await user.click(screen.getByRole("menuitem", { name: "Lock…" }));
    const dialog = screen.getByRole("dialog", { name: "Lock file" });
    await user.type(within(dialog).getByLabelText(/Comment/), "Buoyancy rework");
    await user.click(within(dialog).getByRole("button", { name: "Lock" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(lock).toHaveBeenCalledWith(WS, ["Source/Skyfall/Boat.cpp"], "Buoyancy rework");
    const row = within(pending).getByText("Boat.cpp").closest("li")!;
    expect(within(row).getByText("locked")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /^Locks/ }));
    const locks = await screen.findByRole("region", { name: "Locks" });
    const mine = await within(locks).findByText("Source/Skyfall/Boat.cpp");
    expect(within(mine.closest("tr")!).getByText("you")).toBeInTheDocument();
    expect(within(mine.closest("tr")!).getByText("Buoyancy rework")).toBeInTheDocument();
  });

  it("locks several picked files from the menu at once", async () => {
    const { b, user, pending } = await openSkyfall();
    const lock = vi.spyOn(b, "lock");
    await user.click(within(pending).getByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" }));
    await user.click(within(pending).getByRole("checkbox", { name: "Pick Content/Maps/Harbor.umap" }));
    await user.pointer({ keys: "[MouseRight]", target: within(pending).getByText("Boat.cpp") });
    // Harbor.umap is already mine, so the menu offers both.
    expect(screen.getByRole("menuitem", { name: "Unlock" })).toBeInTheDocument();
    await user.click(screen.getByRole("menuitem", { name: "Lock…" }));
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Lock" }));
    expect(lock).toHaveBeenCalledWith(WS, ["Source/Skyfall/Boat.cpp"], null);
  });

  it("names who holds a file when the lock is refused", async () => {
    const { user, pending } = await openSkyfall();
    await user.click(within(pending).getByText("Crate_02.uasset"));
    const details = screen.getByRole("complementary", { name: "Details" });
    // The file needs a lock, so Lock comes first.
    expect(within(details).getAllByRole("button")[0]).toHaveTextContent("Lock…");
    await user.click(within(details).getByRole("button", { name: "Lock…" }));
    const dialog = screen.getByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Lock" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("locked");
    const holders = await within(dialog).findByRole("list", { name: "Held by others" });
    expect(holders).toHaveTextContent("Content/Props/Crate_02.uasset");
    expect(holders).toHaveTextContent("carol");
    expect(holders).toHaveTextContent("Repainting for the market");
  });

  it("unlocks a file of mine from the details panel", async () => {
    const { b, user, pending } = await openSkyfall();
    const unlock = vi.spyOn(b, "unlock");
    await user.click(within(pending).getByText("Harbor.umap"));
    await user.click(within(screen.getByRole("complementary", { name: "Details" })).getByRole("button", { name: "Unlock" }));
    expect(unlock).toHaveBeenCalledWith(WS, ["Content/Maps/Harbor.umap"], false);
    await waitFor(() => expect(within(within(pending).getByText("Harbor.umap").closest("li")!).queryByText("locked")).toBeNull());
  });

  it("asks before breaking someone else's lock and says when it needs admin", async () => {
    const { user } = await openSkyfall();
    await user.click(screen.getByRole("button", { name: /^Locks/ }));
    const locks = await screen.findByRole("region", { name: "Locks" });
    await user.click(await within(locks).findByRole("button", { name: "Break lock on Content/Maps/Docks.umap" }));
    const dialog = screen.getByRole("dialog", { name: "Break this lock?" });
    expect(dialog).toHaveTextContent("bob");
    await user.click(within(dialog).getByRole("button", { name: "Break lock" }));
    expect(await within(locks).findByRole("alert")).toHaveTextContent("needs admin");
    expect(within(locks).getByText("Content/Maps/Docks.umap")).toBeInTheDocument();
  });

  it("lets a depot admin break a lock", async () => {
    const { b, user } = await openSkyfall({ admin: true });
    const unlock = vi.spyOn(b, "unlock");
    await user.click(screen.getByRole("button", { name: /^Locks/ }));
    const locks = await screen.findByRole("region", { name: "Locks" });
    await user.click(await within(locks).findByRole("button", { name: "Break lock on Content/Maps/Docks.umap" }));
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Break lock" }));
    expect(unlock).toHaveBeenCalledWith(WS, ["Content/Maps/Docks.umap"], true);
    await waitFor(() => expect(within(locks).queryByText("Content/Maps/Docks.umap")).toBeNull());
  });

  it("filters the list and shows only mine", async () => {
    const { user } = await openSkyfall();
    await user.click(screen.getByRole("button", { name: /^Locks/ }));
    const locks = await screen.findByRole("region", { name: "Locks" });
    await within(locks).findByText("Content/Maps/Docks.umap");
    await user.type(within(locks).getByRole("searchbox", { name: "Filter by path" }), "weather");
    expect(within(locks).getAllByRole("row")).toHaveLength(2);
    expect(within(locks).getByText("refactor-bot")).toBeInTheDocument();
    await user.clear(within(locks).getByRole("searchbox", { name: "Filter by path" }));
    await user.click(within(locks).getByRole("checkbox", { name: "Only mine" }));
    expect(within(locks).getAllByRole("row")).toHaveLength(2);
    expect(within(locks).getByText("Content/Maps/Harbor.umap")).toBeInTheDocument();
  });

  it("says when the server has no locks module", async () => {
    const { user } = await openSkyfall({ noLocks: true });
    await user.click(screen.getByRole("button", { name: /^Locks/ }));
    expect(await screen.findByText(/does not have the locks module/)).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
