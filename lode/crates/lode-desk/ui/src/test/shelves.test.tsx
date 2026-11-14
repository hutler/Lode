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

async function openShelves(opts: MockOptions = {}) {
  const o = await openSkyfall(opts);
  await o.user.click(screen.getByRole("button", { name: "Shelves" }));
  const view = await screen.findByRole("region", { name: "Shelves" });
  return { ...o, view };
}

describe("shelving from Changes", () => {
  it("shelves the picked files, adding untracked ones, and reverts them here", async () => {
    const { b, user, pending } = await openSkyfall();
    const shelve = vi.spyOn(b, "shelve");
    const add = vi.spyOn(b, "add");
    await user.click(within(pending).getByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" }));
    await user.click(within(pending).getByRole("checkbox", { name: "Pick notes.txt" }));
    await user.click(within(pending).getByRole("button", { name: "Shelve…" }));
    const dialog = screen.getByRole("dialog", { name: "Shelve 2 files" });
    const go = within(dialog).getByRole("button", { name: "Shelve" });
    expect(go).toBeDisabled();
    await user.type(within(dialog).getByLabelText("Description"), "Tilt experiment, take two");
    await user.click(go);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(add).toHaveBeenCalledWith(WS, ["notes.txt"]);
    expect(shelve).toHaveBeenCalledWith(WS, {
      paths: ["Source/Skyfall/Boat.cpp", "notes.txt"],
      changelist: null,
      message: "Tilt experiment, take two",
      keep: false,
      update: null,
    });
    expect(within(pending).getByRole("status")).toHaveTextContent(
      "Shelved as shelf 45 (2 files); the files were reverted here.",
    );
    await waitFor(() => expect(within(pending).queryByText("Boat.cpp")).toBeNull());
  });

  it("shelves a changelist with its description and keeps the changes", async () => {
    const { b, user, pending } = await openSkyfall();
    const shelve = vi.spyOn(b, "shelve");
    await user.click(within(pending).getByRole("button", { name: "Shelve changelist sails" }));
    const dialog = screen.getByRole("dialog", { name: "Shelve changelist sails" });
    expect(within(dialog).getByLabelText("Description")).toHaveValue("Sail rigging: flap harder when the wind turns");
    await user.click(within(dialog).getByLabelText("Keep my changes in the workspace"));
    await user.click(within(dialog).getByRole("button", { name: "Shelve" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(shelve).toHaveBeenCalledWith(WS, {
      paths: [],
      changelist: "sails",
      message: "Sail rigging: flap harder when the wind turns",
      keep: true,
      update: null,
    });
    expect(within(pending).getByRole("status")).toHaveTextContent("Shelved as shelf 45 (1 file).");
    expect(within(pending).getByText("Sail.cpp")).toBeInTheDocument();
  });

  it("updates an open shelf of mine with new changes", async () => {
    const { b, user, pending } = await openSkyfall();
    const shelve = vi.spyOn(b, "shelve");
    await user.click(within(pending).getByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" }));
    await user.click(within(pending).getByRole("button", { name: "Shelve…" }));
    const dialog = screen.getByRole("dialog", { name: "Shelve 1 file" });
    // Only my own open shelves are offered.
    const put = await within(dialog).findByLabelText(/Put on/);
    expect(within(put).getAllByRole("option").map((o) => o.textContent)).toEqual([
      "A new shelf",
      "Shelf 38 (version 2): Boat tilt experiment: pitch follows the wave slope",
    ]);
    await user.selectOptions(put, "38");
    expect(within(dialog).getByLabelText("Description")).toHaveValue("Boat tilt experiment: pitch follows the wave slope");
    await user.click(within(dialog).getByRole("button", { name: "Update shelf 38" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(shelve.mock.calls[0]![1]).toMatchObject({ update: 38, paths: ["Source/Skyfall/Boat.cpp"] });
    expect(within(pending).getByRole("status")).toHaveTextContent("Updated shelf 38 to version 3 (1 file)");
  });
});

describe("the Shelves view", () => {
  it("lists everyone's open shelves and shows one with its files and diffs", async () => {
    const { user, view } = await openShelves();
    const list = within(view).getByRole("listbox", { name: "Shelf list" });
    const items = await within(list).findAllByRole("option");
    expect(items.map((i) => i.getAttribute("aria-label"))).toEqual([
      "Shelf 44: Rename Buoyancy::SampleWave to SampleHeight",
      "Shelf 42: Market stalls: new awnings and repainted crates",
      "Shelf 38: Boat tilt experiment: pitch follows the wave slope",
    ]);
    // The newest is shown first, with who it is for and each file's diff.
    const shelf = await within(view).findByRole("article", { name: "Shelf 44" });
    expect(within(shelf).getByText("refactor-bot for dave")).toBeInTheDocument();
    expect(within(shelf).getByText(/The old name stays/)).toBeInTheDocument();
    const cpp = within(shelf).getByRole("region", { name: "Diff of Source/Skyfall/Buoyancy.cpp" });
    expect(within(cpp).getByRole("table", { name: "Unified diff" })).toHaveTextContent("SampleHeight");
    // Not mine: no Delete.
    expect(within(shelf).queryByRole("button", { name: "Delete…" })).toBeNull();

    await user.click(items[1]!);
    const market = await within(view).findByRole("article", { name: "Shelf 42" });
    const umap = within(market).getByRole("region", { name: "Diff of Content/Maps/Market.umap" });
    expect(within(umap).getByRole("note")).toHaveTextContent("Too large to compare as text");
    expect(within(umap).getByRole("note")).toHaveTextContent("48.2 MB");
  });

  it("filters to mine and to submitted shelves", async () => {
    const { user, view } = await openShelves();
    await user.click(within(view).getByRole("button", { name: "Mine" }));
    const shelf = await within(view).findByRole("article", { name: "Shelf 38" });
    expect(within(within(view).getByRole("listbox")).getAllByRole("option")).toHaveLength(1);
    expect(within(shelf).getByRole("button", { name: "Delete…" })).toBeInTheDocument();

    await user.selectOptions(within(view).getByLabelText("Shelf state"), "submitted");
    const done = await within(view).findByRole("article", { name: "Shelf 30" });
    expect(within(done).getByText("as r1470")).toBeInTheDocument();
    expect(within(done).queryByRole("button", { name: "Submit…" })).toBeNull();
  });

  it("lists each missing check when required checks refuse the submit", async () => {
    const { user, view } = await openShelves();
    const shelf = await within(view).findByRole("article", { name: "Shelf 44" });
    await user.click(within(shelf).getByRole("button", { name: "Submit…" }));
    const dialog = screen.getByRole("dialog", { name: "Submit shelf 44, version 1" });
    expect(within(dialog).getByLabelText("Message")).toHaveValue(
      "Rename Buoyancy::SampleWave to SampleHeight\n\nThe old name stays as a deprecated wrapper for one release.",
    );
    await user.click(within(dialog).getByRole("button", { name: "Submit" }));
    const missing = await within(dialog).findByRole("list", { name: "Missing checks" });
    const rows = within(missing).getAllByRole("listitem").map((l) => l.textContent);
    expect(rows).toEqual(["ci/build: not reported yet (expected from ci)", "ci/test: failure"]);
    expect(within(dialog).getByRole("alert")).toHaveTextContent("checks_required");
  });

  it("refuses a shelf changed since it was shown and offers the new version", async () => {
    const { b, user, view } = await openShelves();
    await user.click(within(view).getByRole("button", { name: "Mine" }));
    const shelf = await within(view).findByRole("article", { name: "Shelf 38" });
    expect(shelf).toHaveTextContent("version 2");
    b.bumpShelf(WS, 38);
    const submit = vi.spyOn(b, "shelfSubmit");
    await user.click(within(shelf).getByRole("button", { name: "Submit…" }));
    const dialog = screen.getByRole("dialog", { name: "Submit shelf 38, version 2" });
    await user.click(within(dialog).getByRole("button", { name: "Submit" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("it is at version 3 now");
    expect(submit).toHaveBeenCalledWith(WS, 38, 2, null);
    await user.click(within(dialog).getByRole("button", { name: "Review the new version" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(await within(view).findByRole("article", { name: "Shelf 38" })).toHaveTextContent("version 3");
  });

  it("submits a shelf at the version shown", async () => {
    const { b, user, view } = await openShelves();
    const submit = vi.spyOn(b, "shelfSubmit");
    await user.click(await within(view).findByRole("option", { name: /^Shelf 38/ }));
    const shelf = await within(view).findByRole("article", { name: "Shelf 38" });
    await user.click(within(shelf).getByRole("button", { name: "Submit…" }));
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Submit" }));
    expect(await within(view).findByRole("status")).toHaveTextContent("Submitted shelf 38 as r1483 on main.");
    expect(submit).toHaveBeenCalledWith(WS, 38, 2, null);
    await waitFor(() => expect(within(view).queryByRole("option", { name: /^Shelf 38/ })).toBeNull());
  });

  it("unshelves into the workspace with the outcome per file", async () => {
    const { user, view } = await openShelves();
    await user.click(await within(view).findByRole("option", { name: /^Shelf 42/ }));
    const shelf = await within(view).findByRole("article", { name: "Shelf 42" });
    await user.click(within(shelf).getByRole("button", { name: "Unshelve" }));
    const dialog = await screen.findByRole("dialog", { name: "Unshelved shelf 42" });
    const files = within(dialog).getByRole("list", { name: "Unshelved files" });
    const rows = within(files).getAllByRole("listitem").map((l) => l.textContent);
    expect(rows).toEqual([
      "appliedConfig/DefaultGame.ini",
      "appliedContent/Maps/Market.umap",
      "conflictedContent/Props/Crate_02.uasset",
    ]);
    expect(dialog).toHaveTextContent("2 applied, 1 conflicted");
    expect(dialog).toHaveTextContent("lode resolve");
    await user.click(within(dialog).getByRole("button", { name: "Done" }));
    await user.click(screen.getByRole("button", { name: /^Changes/ }));
    const pending = screen.getByRole("region", { name: "Pending changes" });
    expect(await within(pending).findByText("DefaultGame.ini")).toBeInTheDocument();
  });

  it("deletes my shelf after asking", async () => {
    const { b, user, view } = await openShelves();
    const del = vi.spyOn(b, "shelfDelete");
    await user.click(within(view).getByRole("button", { name: "Mine" }));
    const shelf = await within(view).findByRole("article", { name: "Shelf 38" });
    await user.click(within(shelf).getByRole("button", { name: "Delete…" }));
    const dialog = screen.getByRole("dialog", { name: "Delete shelf 38?" });
    await user.click(within(dialog).getByRole("button", { name: "Delete shelf" }));
    expect(await within(view).findByRole("status")).toHaveTextContent("Deleted shelf 38.");
    expect(del).toHaveBeenCalledWith(WS, 38);
    expect(await within(view).findByText(/No open shelves/)).toBeInTheDocument();
  });

  it("says so when the server has no shelves module", async () => {
    const { view } = await openShelves({ noShelves: true });
    expect(await within(view).findByText(/does not have the shelves module/)).toBeInTheDocument();
  });
});
