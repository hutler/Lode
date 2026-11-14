import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { setBackend, type StatusFile } from "../api";
import { App } from "../App";
import { groupFiles } from "../components/ChangesView";
import { createMockBackend } from "../mock";

const WS = "D:\\Work\\Skyfall";

async function openSkyfall() {
  const b = createMockBackend({ stepMs: 0 });
  setBackend(b);
  const user = userEvent.setup();
  render(<App />);
  await user.click(await screen.findByText(WS));
  const pending = await screen.findByRole("region", { name: "Pending changes" });
  await within(pending).findByRole("group", { name: "Changelist sails" });
  return { b, user, pending };
}

function file(path: string, changelist: string | null = null): StatusFile {
  return {
    path,
    status: "modified",
    copy_from: null,
    moved: false,
    changelist,
    locked: false,
    needs_lock: false,
    lock_required: false,
  };
}

/** Drags `from` (a file row) onto `to` (a group). */
function drag(from: HTMLElement, to: HTMLElement) {
  const dataTransfer = { setData: () => {}, effectAllowed: "", dropEffect: "" };
  fireEvent.dragStart(from, { dataTransfer });
  fireEvent.dragOver(to, { dataTransfer });
  fireEvent.drop(to, { dataTransfer });
  fireEvent.dragEnd(from, { dataTransfer });
}

describe("changelist groups", () => {
  it("shows empty changelists and an empty Changes group as places to move to", () => {
    const groups = groupFiles(
      [file("a.cpp", "art")],
      [
        { name: "default", description: "", files: [] },
        { name: "art", description: "Art pass", files: ["a.cpp"] },
        { name: "later", description: "", files: [] },
      ],
    );
    expect(groups.map((g) => [g.title, g.files.length, g.changelist, g.description])).toEqual([
      ["Changes", 0, "default", undefined],
      ["Changelist art", 1, "art", "Art pass"],
      ["Changelist later", 0, "later", undefined],
    ]);
  });
});

describe("changelists", () => {
  it("creates a changelist and moves the picked files into it", async () => {
    const { b, user, pending } = await openSkyfall();
    const move = vi.spyOn(b, "changelistMove");
    await user.click(within(pending).getByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" }));
    await user.click(within(pending).getByRole("checkbox", { name: "Pick notes.txt" }));
    await user.click(within(pending).getByRole("button", { name: "New changelist…" }));
    const dialog = screen.getByRole("dialog", { name: "New changelist" });
    await user.type(within(dialog).getByLabelText(/^Name/), "bad/name");
    expect(within(dialog).getByRole("button", { name: "Create" })).toBeDisabled();
    await user.clear(within(dialog).getByLabelText(/^Name/));
    await user.type(within(dialog).getByLabelText(/^Name/), "buoyancy");
    await user.type(within(dialog).getByLabelText("Description"), "Rough water");
    // notes.txt is not added yet, so only Boat.cpp moves.
    expect(within(dialog).getByLabelText(/Move the 1 picked file/)).toBeChecked();
    await user.click(within(dialog).getByRole("button", { name: "Create" }));
    expect(move).toHaveBeenCalledWith(WS, "buoyancy", ["Source/Skyfall/Boat.cpp"]);
    const group = await within(pending).findByRole("group", { name: "Changelist buoyancy" });
    expect(within(group).getByText("Boat.cpp")).toBeInTheDocument();
    expect(within(group).getByText("Rough water")).toBeInTheDocument();
  });

  it("moves a file by dragging it onto another group", async () => {
    const { b, pending } = await openSkyfall();
    const move = vi.spyOn(b, "changelistMove");
    const boat = within(pending).getByText("Boat.cpp").closest("li")!;
    drag(boat, within(pending).getByRole("group", { name: "Changelist sails" }));
    await waitFor(() => expect(move).toHaveBeenCalledWith(WS, "sails", ["Source/Skyfall/Boat.cpp"]));
    const sails = within(pending).getByRole("group", { name: "Changelist sails" });
    await waitFor(() => expect(within(sails).getByText("Boat.cpp")).toBeInTheDocument());

    // Dropping on the group a file is already in does nothing; new files cannot be dragged.
    move.mockClear();
    drag(within(sails).getByText("Boat.cpp").closest("li")!, sails);
    expect(move).not.toHaveBeenCalled();
    expect(within(pending).getByText("notes.txt").closest("li")).toHaveAttribute("draggable", "false");
  });

  it("moves the picked files with the right-click menu", async () => {
    const { b, user, pending } = await openSkyfall();
    const move = vi.spyOn(b, "changelistMove");
    await user.click(within(pending).getByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" }));
    await user.click(within(pending).getByRole("checkbox", { name: "Pick Content/Maps/Harbor.umap" }));
    await user.pointer({ keys: "[MouseRight]", target: within(pending).getByText("Boat.cpp") });
    expect(screen.getByText("Move 2 files to")).toBeInTheDocument();
    await user.click(screen.getByRole("menuitem", { name: "sails" }));
    expect(move).toHaveBeenCalledWith(WS, "sails", ["Source/Skyfall/Boat.cpp", "Content/Maps/Harbor.umap"]);

    // And back out of it.
    const sails = within(pending).getByRole("group", { name: "Changelist sails" });
    await waitFor(() => expect(within(sails).getByText("Boat.cpp")).toBeInTheDocument());
    await user.pointer({ keys: "[MouseRight]", target: within(sails).getByText("Sail.cpp") });
    await user.click(screen.getByRole("menuitem", { name: "Changes (no changelist)" }));
    expect(move).toHaveBeenLastCalledWith(WS, "default", ["Source/Skyfall/Sail.cpp"]);
  });

  it("commits one changelist with its description as the message", async () => {
    const { b, user, pending } = await openSkyfall();
    const commit = vi.spyOn(b, "commitChangelist");
    // Something else is picked; the changelist commit ignores it.
    await user.click(within(pending).getByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" }));
    await user.click(within(pending).getByRole("button", { name: "Commit changelist sails" }));
    const dialog = screen.getByRole("dialog", { name: "Commit changelist sails" });
    expect(within(dialog).getByLabelText("Message")).toHaveValue("Sail rigging: flap harder when the wind turns");
    await user.click(within(dialog).getByRole("button", { name: "Commit 1 file" }));
    expect(commit).toHaveBeenCalledWith(WS, "sails", "Sail rigging: flap harder when the wind turns");
    expect(await screen.findByRole("status")).toHaveTextContent("Committed r1483 (1 file) from sails.");
    await waitFor(() => expect(within(pending).queryByRole("group", { name: "Changelist sails" })).toBeNull());
    expect(within(pending).getByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" })).toBeChecked();
  });

  it("deletes a changelist after asking, keeping its files", async () => {
    const { user, pending } = await openSkyfall();
    await user.click(within(pending).getByRole("button", { name: "Delete changelist sails" }));
    const dialog = screen.getByRole("dialog", { name: "Delete changelist sails?" });
    expect(dialog).toHaveTextContent("Its 1 file go back to Changes");
    await user.click(within(dialog).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(within(pending).queryByRole("group", { name: "Changelist sails" })).toBeNull());
    const changes = within(pending).getByRole("group", { name: "Changes" });
    expect(within(changes).getByText("Sail.cpp")).toBeInTheDocument();
  });
});
