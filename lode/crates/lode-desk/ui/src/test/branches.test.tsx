import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setBackend } from "../api";
import { App } from "../App";
import { parseRev } from "../components/BranchPicker";
import { createMockBackend, type MockOptions } from "../mock";

const WS = "D:\\Work\\Skyfall";

async function openSkyfall(opts: MockOptions = {}) {
  const b = createMockBackend({ stepMs: 0, ...opts });
  setBackend(b);
  const user = userEvent.setup();
  render(<App />);
  await user.click(await screen.findByText(WS));
  await screen.findByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" });
  return { b, user };
}

async function openPicker(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: /^Branch .*: change branch$/ }));
  const dialog = await screen.findByRole("dialog", { name: "Branches" });
  await within(dialog).findByRole("listbox", { name: "Branches" });
  return dialog;
}

const names = (dialog: HTMLElement) =>
  within(dialog)
    .getAllByRole("option")
    .map((o) => o.querySelector(".branch-name")?.textContent);

afterEach(() => {
  vi.restoreAllMocks();
});

describe("revision fields", () => {
  it("read r1390 or 1390, blank as the head, anything else as invalid", () => {
    expect(parseRev("")).toBeNull();
    expect(parseRev(" r1390 ")).toBe(1390);
    expect(parseRev("1390")).toBe(1390);
    expect(parseRev("head")).toBeNaN();
    expect(parseRev("r-1")).toBeNaN();
  });
});

describe("the branch picker", () => {
  it("lists active branches with head and origin, the current first, and filters them", async () => {
    const { user } = await openSkyfall();
    const dialog = await openPicker(user);
    expect(names(dialog)).toEqual(["main", "art/harbor-night", "release-1.2"]);
    const main = within(dialog).getByRole("option", { name: /^main/ });
    expect(main).toHaveAttribute("aria-selected", "true");
    expect(main).toHaveTextContent("current");
    expect(main).toHaveTextContent("r1482 · first branch");
    expect(within(dialog).getByRole("option", { name: /^release-1\.2/ })).toHaveTextContent("r1404 · from main@r1390");
    // Nothing to switch to on the current branch.
    expect(within(dialog).queryByRole("button", { name: "Switch" })).toBeNull();

    await user.type(within(dialog).getByRole("searchbox", { name: "Filter branches" }), "REL");
    expect(names(dialog)).toEqual(["release-1.2"]);
    await user.clear(within(dialog).getByRole("searchbox", { name: "Filter branches" }));

    await user.click(within(dialog).getByRole("checkbox", { name: "Show archived" }));
    await waitFor(() => expect(names(dialog)).toContain("proto/boats"));
    expect(names(dialog).at(-1)).toBe("proto/boats");
    expect(within(dialog).getByRole("option", { name: /^proto\/boats/ })).toHaveTextContent("archived");
  });

  it("checks the revision before switching", async () => {
    const { user } = await openSkyfall();
    const dialog = await openPicker(user);
    await user.click(within(dialog).getByRole("option", { name: /^release-1\.2/ }));
    const rev = within(dialog).getByRole("textbox", { name: "Revision" });
    expect(rev).toHaveAttribute("placeholder", "head (r1404)");
    await user.type(rev, "r1500");
    expect(within(dialog).getByRole("button", { name: "Switch" })).toBeDisabled();
    await user.clear(rev);
    await user.type(rev, "r1400");
    expect(within(dialog).getByRole("button", { name: "Switch" })).toBeEnabled();
  });
});

describe("switching", () => {
  it("carries local changes over, shows conflicts and the locks left on the old branch, and every view follows", async () => {
    const { b, user } = await openSkyfall();
    const log = vi.spyOn(b, "log");
    const dialog = await openPicker(user);
    await user.click(within(dialog).getByRole("option", { name: /^release-1\.2/ }));
    await user.click(within(dialog).getByRole("button", { name: "Switch" }));

    const result = await screen.findByRole("dialog", { name: "Switched to release-1.2" });
    expect(result).toHaveTextContent("The workspace is on release-1.2 at r1404, from main.");
    const files = within(result).getByRole("list", { name: "Files" });
    expect(within(files).getByText("Source/Skyfall/Boat.cpp").previousSibling).toHaveTextContent("conflicted");
    expect(within(files).getByText("Content/Maps/Harbor.umap").previousSibling).toHaveTextContent("merged");
    expect(within(result).getByRole("note", { name: "Conflicts" })).toHaveTextContent("1 file did not merge");
    const left = within(result).getByRole("note", { name: "Locks left behind" });
    expect(left).toHaveTextContent("Your locks stay on main.");
    expect(within(left).getByText("Content/Maps/Harbor.umap")).toBeInTheDocument();

    await user.click(within(result).getByRole("button", { name: "Show changes" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    // The sidebar, the tab and the Changes view are on the new branch.
    expect(screen.getByRole("button", { name: "Branch release-1.2 at r1404: change branch" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^skyfall/ })).toHaveTextContent("release-1.2");
    const boat = await screen.findByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" });
    expect(boat.parentElement?.querySelector(".st")).toHaveAttribute("title", "Conflicted");

    // Locks: the lock on Harbor.umap stayed on main, so nothing is held here.
    await user.click(screen.getByRole("button", { name: "Locks" }));
    await screen.findByRole("region", { name: "Locks" });
    expect(screen.queryByText("Content/Maps/Harbor.umap")).toBeNull();

    // History shows the new branch.
    await user.click(screen.getByRole("button", { name: "History" }));
    await screen.findAllByRole("option", { name: /^r1404 / });
    expect(log).toHaveBeenLastCalledWith(WS, null, null, expect.any(Number));
    expect(screen.queryByRole("option", { name: /^r1482 / })).toBeNull();
  });
});

describe("creating a branch", () => {
  it("from another branch at a revision, switching to it", async () => {
    const { b, user } = await openSkyfall();
    const create = vi.spyOn(b, "branchCreate");
    const picker = await openPicker(user);
    await user.click(within(picker).getByRole("button", { name: "New branch…" }));
    const dialog = await screen.findByRole("dialog", { name: "New branch" });
    expect(within(dialog).getByRole("radio", { name: "This workspace: main at r1482" })).toBeChecked();
    await user.type(within(dialog).getByRole("textbox", { name: /^Name/ }), "fix/docks");
    await user.click(within(dialog).getByRole("radio", { name: "Another branch or revision" }));
    await user.selectOptions(within(dialog).getByRole("combobox", { name: "Branch to start from" }), "release-1.2");
    await user.type(within(dialog).getByRole("textbox", { name: "Revision to start from" }), "1400");
    expect(within(dialog).getByRole("checkbox", { name: "Switch this workspace to it" })).toBeChecked();
    await user.click(within(dialog).getByRole("button", { name: "Create and switch" }));

    expect(create).toHaveBeenCalledWith(WS, "fix/docks", "release-1.2@r1400", true);
    const result = await screen.findByRole("dialog", { name: "Switched to fix/docks" });
    expect(result).toHaveTextContent("at r1400, from main");
    await user.click(within(result).getByRole("button", { name: "Done" }));
    expect(screen.getByRole("button", { name: "Branch fix/docks at r1400: change branch" })).toBeInTheDocument();

    const again = await openPicker(user);
    expect(within(again).getByRole("option", { name: /^fix\/docks/ })).toHaveTextContent(
      "r1400 · from release-1.2@r1400",
    );
  });

  it("without switching, from the workspace, and refuses a name that exists", async () => {
    const { b, user } = await openSkyfall();
    const create = vi.spyOn(b, "branchCreate");
    const picker = await openPicker(user);
    await user.click(within(picker).getByRole("button", { name: "New branch…" }));
    let dialog = await screen.findByRole("dialog", { name: "New branch" });
    await user.type(within(dialog).getByRole("textbox", { name: /^Name/ }), "release-1.2");
    await user.click(within(dialog).getByRole("checkbox", { name: "Switch this workspace to it" }));
    await user.click(within(dialog).getByRole("button", { name: "Create branch" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("branch `release-1.2` already exists");

    await user.clear(within(dialog).getByRole("textbox", { name: /^Name/ }));
    await user.type(within(dialog).getByRole("textbox", { name: /^Name/ }), "art/sky");
    await user.click(within(dialog).getByRole("button", { name: "Create branch" }));
    expect(create).toHaveBeenLastCalledWith(WS, "art/sky", null, false);

    // Back in the picker with the new branch selected; the workspace stayed on main.
    dialog = await screen.findByRole("dialog", { name: "Branches" });
    const made = await within(dialog).findByRole("option", { name: /^art\/sky/ });
    expect(made).toHaveAttribute("aria-selected", "true");
    expect(made).toHaveTextContent("from main@r1482");
    expect(screen.getByRole("button", { name: "Branch main at r1482: change branch" })).toBeInTheDocument();
  });
});

describe("archiving", () => {
  it("asks first, and needs depot admin", async () => {
    const { user } = await openSkyfall();
    const dialog = await openPicker(user);
    await user.click(within(dialog).getByRole("option", { name: /^art\/harbor-night/ }));
    await user.click(within(dialog).getByRole("button", { name: "Archive…" }));
    const ask = await screen.findByRole("dialog", { name: "Archive art/harbor-night?" });
    await user.click(within(ask).getByRole("button", { name: "Archive branch" }));
    const again = await screen.findByRole("dialog", { name: "Branches" });
    const alert = await within(again).findByRole("alert");
    expect(alert).toHaveTextContent("archiving a branch needs admin on the depot");
    expect(alert).toHaveTextContent("Ask a depot admin for access.");
  });

  it("hides the branch unless archived ones are shown", async () => {
    const { user } = await openSkyfall({ admin: true });
    let dialog = await openPicker(user);
    // The current branch cannot be archived from here.
    expect(within(dialog).queryByRole("button", { name: "Archive…" })).toBeNull();
    await user.click(within(dialog).getByRole("option", { name: /^art\/harbor-night/ }));
    await user.click(within(dialog).getByRole("button", { name: "Archive…" }));
    const ask = await screen.findByRole("dialog", { name: "Archive art/harbor-night?" });
    expect(ask).toHaveTextContent("nobody can commit to it any more");
    await user.click(within(ask).getByRole("button", { name: "Archive branch" }));
    dialog = await screen.findByRole("dialog", { name: "Branches" });
    await waitFor(() => expect(names(dialog)).toEqual(["main", "release-1.2"]));
    await user.click(within(dialog).getByRole("checkbox", { name: "Show archived" }));
    await waitFor(() => expect(names(dialog)).toEqual(["main", "release-1.2", "art/harbor-night", "proto/boats"]));
  });
});

describe("an update that stopped", () => {
  it("says to finish it and finishes it with one button", async () => {
    const { b, user } = await openSkyfall({ unfinishedUpdate: true });
    const update = vi.spyOn(b, "update");
    await user.click(screen.getByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" }));
    await user.type(screen.getByLabelText("Commit message"), "Boat");
    await user.click(screen.getByRole("button", { name: "Commit 1 file" }));
    const alert = await within(screen.getByRole("form", { name: "Commit" })).findByRole("alert");
    expect(alert).toHaveTextContent("an update did not finish");
    expect(alert).toHaveTextContent("Finish it, then try again.");

    await user.click(within(alert).getByRole("button", { name: "Finish the update" }));
    expect(update).toHaveBeenCalledWith(WS, null);
    const result = await screen.findByRole("dialog", { name: "Updated to r1482" });
    expect(within(result).getByRole("list", { name: "Files" })).toHaveTextContent("Config/DefaultGame.ini");
    await user.click(within(result).getByRole("button", { name: "Done" }));

    // Now the commit goes through.
    await user.click(await screen.findByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" }));
    await user.type(screen.getByLabelText("Commit message"), "Boat");
    await user.click(screen.getByRole("button", { name: "Commit 1 file" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Committed r1483 (1 file).");
  });

  it("offers the same button when a switch is refused for it", async () => {
    const { user } = await openSkyfall({ unfinishedUpdate: true });
    const dialog = await openPicker(user);
    await user.click(within(dialog).getByRole("option", { name: /^release-1\.2/ }));
    await user.click(within(dialog).getByRole("button", { name: "Switch" }));
    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent("update_incomplete");
    await user.click(within(alert).getByRole("button", { name: "Finish the update" }));
    expect(await screen.findByRole("dialog", { name: "Updated to r1482" })).toBeInTheDocument();
  });
});

describe("updating", () => {
  it("brings the workspace to the head of its branch from the picker", async () => {
    const { b, user } = await openSkyfall();
    const update = vi.spyOn(b, "update");
    const dialog = await openPicker(user);
    expect(within(dialog).queryByRole("button", { name: "Switch" })).toBeNull();
    await user.click(within(dialog).getByRole("button", { name: "Update to r1482" }));
    expect(update).toHaveBeenCalledWith(WS, null);
    expect(await screen.findByRole("dialog", { name: /^Updated to r\d+$/ })).toBeInTheDocument();
  });
});
