import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setBackend } from "../api";
import { App } from "../App";
import { mergeMessage } from "../components/ChangesView";
import { changedLines, countConflicts, lines, parseConflicts, takeRegion } from "../merge";
import { createMockBackend, type MockBackend } from "../mock";

const WS = "D:\\Work\\Skyfall";
const BOAT = "Source/Skyfall/Boat.cpp";
const HARBOR = "Content/Maps/Harbor.umap";

type User = ReturnType<typeof userEvent.setup>;

async function openSkyfall(prepare?: (b: MockBackend) => Promise<void>) {
  const b = createMockBackend({ stepMs: 0 });
  await prepare?.(b);
  setBackend(b);
  const user = userEvent.setup();
  render(<App />);
  await user.click(await screen.findByText(WS));
  await screen.findByRole("checkbox", { name: `Pick ${BOAT}` });
  return { b, user };
}

/** Merges release-1.2 into main through the picker; `rev` picks one revision. */
async function mergeRelease(user: User, rev?: string) {
  await user.click(screen.getByRole("button", { name: /^Branch .*: change branch$/ }));
  const picker = await screen.findByRole("dialog", { name: "Branches" });
  await user.click(await within(picker).findByRole("option", { name: /^release-1\.2/ }));
  await user.click(within(picker).getByRole("button", { name: "Merge…" }));
  const dialog = await screen.findByRole("dialog", { name: "Merge into main" });
  expect(within(dialog).getByRole("combobox", { name: "Merge from" })).toHaveValue("release-1.2");
  if (rev) {
    await user.click(within(dialog).getByRole("radio", { name: "Only one revision" }));
    await user.type(within(dialog).getByRole("textbox", { name: "Revision to merge" }), rev);
  }
  await user.click(within(dialog).getByRole("button", { name: "Merge" }));
}

async function resolveFile(user: User, path: string) {
  await user.click(await screen.findByText(path.slice(path.lastIndexOf("/") + 1)));
  await user.click(within(screen.getByRole("complementary", { name: "Details" })).getByRole("button", { name: "Resolve…" }));
  return screen.findByRole("region", { name: "Resolve conflict" });
}

afterEach(() => {
  vi.restoreAllMocks();
});

const MARKED = [
  "one",
  "<<<<<<< mine",
  "two mine",
  "=======",
  "two theirs",
  ">>>>>>> theirs r9",
  "three",
  "<<<<<<< mine",
  "four mine",
  "|||||||",
  "four",
  "=======",
  ">>>>>>> theirs r9",
  "",
].join("\n");

describe("conflict markers", () => {
  it("split a file into plain runs and regions, with labels and an optional base", () => {
    const parts = parseConflicts(MARKED);
    expect(parts.map((p) => p.kind)).toEqual(["text", "conflict", "text", "conflict"]);
    expect(parts[1]).toMatchObject({ mine: ["two mine"], theirs: ["two theirs"], base: null, theirsLabel: "theirs r9" });
    expect(parts[3]).toMatchObject({ mine: ["four mine"], base: ["four"], theirs: [] });
    expect(countConflicts(MARKED)).toBe(2);
    // An unclosed marker is plain text.
    expect(countConflicts("a\n<<<<<<< mine\nb\n")).toBe(0);
  });

  it("take one region from either side, or both, keeping the rest and the line ends", () => {
    const t = takeRegion(MARKED, 0, "theirs");
    expect(countConflicts(t)).toBe(1);
    expect(t.startsWith("one\ntwo theirs\nthree\n<<<<<<< mine\n")).toBe(true);
    expect(takeRegion(t, 0, "mine")).toBe("one\ntwo theirs\nthree\nfour mine\n");
    expect(takeRegion(MARKED, 1, "both").endsWith("three\nfour mine\n")).toBe(true);
    const crlf = MARKED.replace(/\n/g, "\r\n");
    expect(takeRegion(takeRegion(crlf, 0, "mine"), 0, "theirs")).toBe("one\r\ntwo mine\r\nthree\r\n");
  });

  it("mark the lines a side changed against the base", () => {
    expect([...changedLines(lines("a\nb\nc\n"), lines("a\nB\nc\nd\n"))!]).toEqual([1, 3]);
    expect(changedLines(["x"], ["x"])!.size).toBe(0);
    expect(changedLines(["a", "b"], ["c", "d"], 1)).toBeNull();
  });

  it("suggest the merge's commit message", () => {
    expect(mergeMessage([{ branch: "release-1.2", from_rev: 1391, to_rev: 1404 }])).toBe("Merge release-1.2 r1391..r1404");
    expect(mergeMessage([{ branch: "hotfix", from_rev: 7, to_rev: 7 }])).toBe("Merge hotfix r7");
  });
});

describe("merging", () => {
  it("lists each file's outcome, then shows the pending merge in Changes with commit held back", async () => {
    const { b, user } = await openSkyfall();
    const merge = vi.spyOn(b, "merge");
    await mergeRelease(user);
    expect(merge).toHaveBeenCalledWith(WS, "release-1.2", null);
    const result = await screen.findByRole("dialog", { name: "Merged release-1.2 r1391..r1404" });
    expect(result).toHaveTextContent("2 applied, 1 merged, 2 conflicted, 1 unchanged");
    const files = within(result).getByRole("list", { name: "Files" });
    expect(within(files).getByText(BOAT).previousSibling).toHaveTextContent("conflicted");
    expect(within(files).getByText("Source/Skyfall/Wake.cpp").previousSibling).toHaveTextContent("applied");
    expect(within(result).getByRole("note", { name: "Conflicts" })).toHaveTextContent("2 files changed on both sides");
    await user.click(within(result).getByRole("button", { name: "Resolve conflicts" }));

    const note = await screen.findByRole("note", { name: "Pending merge" });
    expect(note).toHaveTextContent("Merging release-1.2 r1391..r1404.");
    expect(screen.getByLabelText("Commit message")).toHaveValue("Merge release-1.2 r1391..r1404");
    const commit = screen.getByRole("button", { name: "Commit merge" });
    expect(commit).toBeDisabled();
    expect(commit).toHaveAccessibleDescription("Resolve 2 conflicted files before you commit.");
    expect(screen.getByRole("button", { name: "Commit changelist sails" })).toBeDisabled();
  });

  it("can pick one revision", async () => {
    const { b, user } = await openSkyfall();
    const merge = vi.spyOn(b, "merge");
    await mergeRelease(user, "r1400");
    expect(merge).toHaveBeenCalledWith(WS, "release-1.2", 1400);
    const result = await screen.findByRole("dialog", { name: "Merged release-1.2 r1400" });
    expect(within(result).queryByRole("note", { name: "Conflicts" })).toBeNull();
    await user.click(within(result).getByRole("button", { name: "Done" }));
    expect(await screen.findByRole("note", { name: "Pending merge" })).toHaveTextContent("Merging release-1.2 r1400.");
  });

  it("abandons the merge after asking, keeping the files", async () => {
    const { b, user } = await openSkyfall();
    const abandon = vi.spyOn(b, "abandonMerge");
    await mergeRelease(user, "1400");
    await user.click(within(await screen.findByRole("dialog", { name: /^Merged/ })).getByRole("button", { name: "Done" }));
    await user.click(await screen.findByRole("button", { name: "Abandon merge…" }));
    const ask = screen.getByRole("dialog", { name: "Abandon the merge?" });
    expect(ask).toHaveTextContent("The files stay as they are");
    await user.click(within(ask).getByRole("button", { name: "Abandon merge" }));
    expect(abandon).toHaveBeenCalledWith(WS);
    await waitFor(() => expect(screen.queryByRole("note", { name: "Pending merge" })).toBeNull());
    expect(screen.getByLabelText("Commit message")).toHaveValue("");
    expect(screen.getByText("Weather.cpp")).toBeInTheDocument();
  });

  it("commits the merge whole once the conflicts are resolved", async () => {
    const { b, user } = await openSkyfall(async (b) => {
      await b.revert(WS, ["Content/Props/Crate_02.uasset"]);
    });
    const commitAll = vi.spyOn(b, "commitAll");
    await mergeRelease(user);
    await user.click(within(await screen.findByRole("dialog", { name: /^Merged/ })).getByRole("button", { name: "Resolve conflicts" }));
    for (const path of [BOAT, HARBOR]) {
      const view = await resolveFile(user, path);
      await user.click(within(view).getByRole("button", { name: path === BOAT ? "Take mine" : "Keep mine" }));
      await screen.findByRole("note", { name: "Pending merge" });
    }
    const commit = screen.getByRole("button", { name: "Commit merge" });
    await waitFor(() => expect(commit).toBeEnabled());
    await user.click(commit);
    expect(commitAll).toHaveBeenCalledWith(WS, "Merge release-1.2 r1391..r1404");
    expect(await screen.findByRole("status")).toHaveTextContent(/^Committed r1483/);
    expect(screen.queryByRole("note", { name: "Pending merge" })).toBeNull();
  });
});

describe("the three-way view", () => {
  it("shows base, mine and theirs with changes marked, and takes one region from each side", async () => {
    const { b, user } = await openSkyfall();
    const resolveWith = vi.spyOn(b, "resolveWith");
    await mergeRelease(user);
    await user.click(within(await screen.findByRole("dialog", { name: /^Merged/ })).getByRole("button", { name: "Resolve conflicts" }));
    const view = await resolveFile(user, BOAT);
    expect(within(view).getByRole("heading", { name: "Resolve Boat.cpp" })).toBeInTheDocument();
    expect(view).toHaveTextContent("Theirs came from r1404");
    const pane = (name: string | RegExp) => within(view).getByRole("figure", { name });
    expect(pane("Base").querySelectorAll("[data-changed]")).toHaveLength(0);
    const mineChanged = [...pane("Mine").querySelectorAll("[data-changed]")].map((l) => l.textContent);
    expect(mineChanged).toEqual([
      expect.stringContaining("Wave * 0.35f"),
      expect.stringContaining("Pitch ="),
      expect.stringContaining("MaxSail"),
    ]);
    expect([...pane(/^Theirs/).querySelectorAll("[data-changed]")].map((l) => l.textContent)).toEqual([
      expect.stringContaining("UnrealNetwork.h"),
      expect.stringContaining("RollScale"),
      expect.stringContaining("OnSailChanged"),
    ]);

    const result = within(view).getByRole("textbox", { name: "Result" });
    expect((result as HTMLTextAreaElement).value).toContain("<<<<<<< mine");
    const save = within(view).getByRole("button", { name: "Save and mark resolved" });
    expect(save).toBeDisabled();
    expect(save).toHaveAccessibleDescription("Resolve every conflict first.");

    await user.click(within(within(view).getByRole("group", { name: "Conflict 1 of 2" })).getByRole("button", { name: "Use mine" }));
    const last = within(view).getByRole("group", { name: "Conflict 1 of 1" });
    expect(within(last).getByLabelText("Theirs")).toHaveTextContent("OnSailChanged.Broadcast(Sail);");
    await user.click(within(last).getByRole("button", { name: "Use theirs" }));
    const text = (result as HTMLTextAreaElement).value;
    expect(countConflicts(text)).toBe(0);
    expect(text).toContain("Wave * 0.35f");
    expect(text).toContain("OnSailChanged.Broadcast(Sail);");
    expect(text).toContain('#include "Net/UnrealNetwork.h"');
    expect(view).toHaveTextContent("No conflicts left.");

    await user.click(save);
    expect(resolveWith).toHaveBeenCalledWith(WS, BOAT, text);
    expect(await screen.findByRole("status")).toHaveTextContent(`Resolved ${BOAT}: saved the edited result.`);
    expect(screen.getByRole("button", { name: "Commit merge" })).toHaveAccessibleDescription(
      "Resolve the conflicted file before you commit.",
    );
  });

  it("offers mine or theirs with each side's size for a binary file", async () => {
    const { b, user } = await openSkyfall();
    const resolve = vi.spyOn(b, "resolve");
    await mergeRelease(user);
    await user.click(within(await screen.findByRole("dialog", { name: /^Merged/ })).getByRole("button", { name: "Resolve conflicts" }));
    const view = await resolveFile(user, HARBOR);
    expect(within(view).queryByRole("textbox", { name: "Result" })).toBeNull();
    const mine = within(view).getByRole("group", { name: "Mine" });
    const theirs = within(view).getByRole("group", { name: "Theirs" });
    expect(mine).toHaveTextContent("48.2 MB");
    expect(theirs).toHaveTextContent("48.5 MB");
    expect(theirs).toHaveTextContent(`${HARBOR}.theirs-r1404`);
    await user.click(within(theirs).getByRole("button", { name: "Take theirs" }));
    expect(resolve).toHaveBeenCalledWith(WS, [HARBOR], "theirs");
    expect(await screen.findByRole("status")).toHaveTextContent(`Resolved ${HARBOR}: took their version.`);
  });

  it("opens from a double click on a conflicted file", async () => {
    const { user } = await openSkyfall();
    await mergeRelease(user);
    await user.click(within(await screen.findByRole("dialog", { name: /^Merged/ })).getByRole("button", { name: "Resolve conflicts" }));
    await user.dblClick(await screen.findByText("Boat.cpp"));
    expect(await screen.findByRole("heading", { name: "Resolve Boat.cpp" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "← Changes" }));
    expect(await screen.findByRole("note", { name: "Pending merge" })).toBeInTheDocument();
  });
});
