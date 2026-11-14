import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { setBackend, type StatusFile } from "../api";
import { App } from "../App";
import { groupFiles } from "../components/ChangesView";
import { createMockBackend, type MockOptions } from "../mock";

const WS = "D:\\Work\\Skyfall";

function file(path: string, status: StatusFile["status"], extra: Partial<StatusFile> = {}): StatusFile {
  return {
    path,
    status,
    copy_from: null,
    moved: false,
    changelist: null,
    locked: false,
    needs_lock: false,
    lock_required: false,
    ...extra,
  };
}

async function openSkyfall(opts: MockOptions = {}) {
  const b = createMockBackend({ stepMs: 0, ...opts });
  setBackend(b);
  const user = userEvent.setup();
  render(<App />);
  await user.click(await screen.findByText(WS));
  await screen.findByRole("group", { name: "Changes" });
  return { b, user };
}

describe("grouping", () => {
  it("lists plain changes, then changelists by name, then files not added", () => {
    const groups = groupFiles([
      file("z.txt", "untracked"),
      file("b.cpp", "modified", { changelist: "zeta" }),
      file("a.cpp", "modified"),
      file("c.cpp", "added", { changelist: "alpha" }),
      file("same.txt", "unchanged"),
    ]);
    expect(groups.map((g) => [g.title, g.files.map((f) => f.path)])).toEqual([
      ["Changes", ["a.cpp"]],
      ["Changelist alpha", ["c.cpp"]],
      ["Changelist zeta", ["b.cpp"]],
      ["Not added yet", ["z.txt"]],
    ]);
  });

  it("shows each group with its files, markers and counts", async () => {
    await openSkyfall();
    const main = screen.getByRole("group", { name: "Changes" });
    expect(within(main).getAllByRole("listitem")).toHaveLength(3);
    expect(within(main).getByText("Harbor.umap").closest("li")).toHaveTextContent("locked");
    expect(within(main).getByText("Crate_02.uasset").closest("li")).toHaveTextContent("needs lock");
    const cl = screen.getByRole("group", { name: "Changelist sails" });
    expect(within(cl).getByText("Sail.cpp")).toBeInTheDocument();
    const fresh = screen.getByRole("group", { name: "Not added yet" });
    expect(within(fresh).getByText("notes.txt")).toBeInTheDocument();
  });
});

describe("picking and committing", () => {
  it("picks a whole group, counts it on the button and needs a message", async () => {
    const { user } = await openSkyfall();
    const commit = screen.getByRole("button", { name: /^Commit \d+ file/ });
    expect(commit).toHaveTextContent("Commit 0 files");
    expect(commit).toBeDisabled();
    await user.click(screen.getByRole("checkbox", { name: "Pick all of Changes" }));
    expect(commit).toHaveTextContent("Commit 3 files");
    expect(commit).toBeDisabled();
    await user.type(screen.getByLabelText("Commit message"), "Harbor fixes");
    expect(commit).toBeEnabled();
    await user.click(screen.getByRole("checkbox", { name: "Pick Content/Maps/Harbor.umap" }));
    expect(commit).toHaveTextContent("Commit 2 files");
  });

  it("commits the picked files, adding new ones first, and clears the box", async () => {
    const { b, user } = await openSkyfall();
    await user.click(screen.getByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" }));
    await user.click(screen.getByRole("checkbox", { name: "Pick notes.txt" }));
    await user.type(screen.getByLabelText("Commit message"), "Boat and notes");
    await user.click(screen.getByRole("button", { name: "Commit 2 files" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Committed r1483 (2 files).");
    expect(screen.getByLabelText("Commit message")).toHaveValue("");
    const left = (await b.status(WS)).files.map((f) => f.path);
    expect(left).not.toContain("Source/Skyfall/Boat.cpp");
    expect(left).not.toContain("notes.txt");
    expect(screen.queryByText("Boat.cpp")).not.toBeInTheDocument();
  });

  it("shows why the server refused and which files", async () => {
    const { user } = await openSkyfall();
    await user.click(screen.getByRole("checkbox", { name: "Pick Content/Props/Crate_02.uasset" }));
    await user.type(screen.getByLabelText("Commit message"), "Crate");
    await user.click(screen.getByRole("button", { name: "Commit 1 file" }));
    const form = screen.getByRole("form", { name: "Commit" });
    const alert = await within(form).findByRole("alert");
    expect(alert).toHaveTextContent("these files need a lock");
    expect(alert).toHaveTextContent("lock_required");
    expect(within(form).getByRole("list", { name: "Files the server refused" })).toHaveTextContent(
      "Content/Props/Crate_02.uasset",
    );
    // Nothing was committed and the pick is kept for another try.
    expect(screen.getByRole("checkbox", { name: "Pick Content/Props/Crate_02.uasset" })).toBeChecked();
    expect(screen.getByLabelText("Commit message")).toHaveValue("Crate");
  });

  it("shows an out-of-date refusal with the next step", async () => {
    const { user } = await openSkyfall({
      refuseCommit: {
        code: "out_of_date",
        message:
          "out of date: Source/Skyfall/Boat.cpp; these files changed on the server since you synced them: run `lode update`, then commit again",
        details: { paths: ["Source/Skyfall/Boat.cpp"] },
      },
    });
    await user.click(screen.getByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" }));
    await user.type(screen.getByLabelText("Commit message"), "Boat");
    await user.click(screen.getByRole("button", { name: "Commit 1 file" }));
    const alert = await within(screen.getByRole("form", { name: "Commit" })).findByRole("alert");
    expect(alert).toHaveTextContent("then commit again");
    expect(alert).toHaveTextContent("out_of_date");
  });
});

describe("add, revert and refresh", () => {
  it("adds a new file from the right-click menu", async () => {
    const { user } = await openSkyfall();
    await user.pointer({ keys: "[MouseRight]", target: screen.getByText("notes.txt") });
    await user.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Add" }));
    await waitFor(() =>
      expect(within(screen.getByRole("group", { name: "Changes" })).getByText("notes.txt")).toBeInTheDocument(),
    );
    expect(screen.queryByRole("group", { name: "Not added yet" })).not.toBeInTheDocument();
  });

  it("asks before reverting and keeps the file when cancelled", async () => {
    const { b, user } = await openSkyfall();
    const list = () => screen.getByRole("region", { name: "Pending changes" });
    await user.pointer({ keys: "[MouseRight]", target: within(list()).getByText("Boat.cpp") });
    await user.click(screen.getByRole("menuitem", { name: "Revert…" }));
    const dialog = screen.getByRole("dialog", { name: "Revert changes?" });
    expect(dialog).toHaveTextContent("Source/Skyfall/Boat.cpp");
    expect(dialog).toHaveTextContent("cannot be undone");
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(within(list()).getByText("Boat.cpp")).toBeInTheDocument();

    await user.pointer({ keys: "[MouseRight]", target: within(list()).getByText("Boat.cpp") });
    await user.click(screen.getByRole("menuitem", { name: "Revert…" }));
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Revert" }));
    await waitFor(() => expect(within(list()).queryByText("Boat.cpp")).not.toBeInTheDocument());
    expect((await b.status(WS)).files.map((f) => f.path)).not.toContain("Source/Skyfall/Boat.cpp");
  });

  it("refreshes when files change on disk", async () => {
    const { b } = await openSkyfall();
    b.touch(WS, file("Config/DefaultGame.ini", "modified"));
    expect(await screen.findByText("DefaultGame.ini")).toBeInTheDocument();
  });

  it("shows details of the selected file", async () => {
    const { user } = await openSkyfall();
    await user.click(screen.getByText("Crate_02.uasset"));
    const details = screen.getByRole("complementary", { name: "Details" });
    expect(details).toHaveTextContent("Content/Props/Crate_02.uasset");
    expect(details).toHaveTextContent("Needs a lock before it can be committed");
    expect(within(details).getByRole("button", { name: "Revert…" })).toBeInTheDocument();
  });
});
