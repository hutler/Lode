import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { setBackend } from "../api";
import { App } from "../App";
import { layoutGraph } from "../graph";
import { createMockBackend } from "../mock";
import { demoLog } from "../mockHistory";

const WS = "D:\\Work\\Skyfall";

async function openHistory(workspace = WS) {
  const b = createMockBackend({ stepMs: 0 });
  const log = vi.spyOn(b, "log");
  setBackend(b);
  const user = userEvent.setup();
  render(<App />);
  await user.click(await screen.findByText(workspace));
  await user.click(await screen.findByRole("button", { name: "History" }));
  await screen.findAllByRole("option", { name: /^r\d+ / });
  return { b, log, user };
}

describe("graph layout", () => {
  it("draws a merge edge on each merging revision of a branch with two merges", () => {
    const { revisions } = demoLog("main", null, 1480, 40);
    const g = layoutGraph(revisions, "main", true);
    expect(g.lanes).toBe(2);
    const merging = g.rows.filter((r) => r.merges.length > 0);
    expect(merging.map((r) => r.rev)).toEqual([1478, 1451]);
    expect(merging[0]?.merges[0]).toEqual({ branch: "feature/harbor-night", from_rev: 1461, to_rev: 1474 });
    expect(g.rows.every((r) => !r.inherited && !r.branchPoint)).toBe(true);
    expect(g.rows[0]?.lineAbove).toBe(false);
    expect(g.rows[1]?.lineAbove).toBe(true);
    // More revisions follow, so the line goes on below the last loaded one.
    expect(g.rows[g.rows.length - 1]?.lineBelow).toBe(true);
  });

  it("uses one lane without merges and ends the line at the oldest revision", () => {
    const { revisions, more } = demoLog("main", null, 6, 10);
    expect(more).toBe(false);
    const g = layoutGraph(revisions, "main", more);
    expect(g.lanes).toBe(1);
    expect(g.rows.map((r) => r.lineBelow)).toEqual([true, true, true, true, false]);
  });

  it("marks where a branch's history becomes its parent's", () => {
    const { revisions } = demoLog("release-1.2", null, 1394, 6);
    const g = layoutGraph(revisions, "release-1.2");
    expect(g.rows.map((r) => [r.rev, r.inherited, r.branchPoint])).toEqual([
      [1393, false, false],
      [1392, false, false],
      [1391, false, false],
      [1390, true, true],
      [1389, true, false],
      [1388, true, false],
    ]);
    expect(g.rows[3]?.aboveInherited).toBe(false);
    expect(g.rows[4]?.aboveInherited).toBe(true);
  });
});

describe("history view", () => {
  it("lists the branch's revisions newest first with badges", async () => {
    await openHistory();
    const list = screen.getByRole("listbox", { name: "Revisions" });
    const first = within(list).getAllByRole("option")[0];
    expect(first).toHaveAccessibleName(/^r1482 /);
    const merge = within(list).getByRole("option", { name: /^r1478 / });
    expect(within(merge).getByText("merge feature/harbor-night")).toBeInTheDocument();
    const agent = within(list).getByRole("option", { name: /^r1474 / });
    expect(within(agent).getByText("agent")).toBeInTheDocument();
    expect(within(agent).getByText("refactor-bot")).toBeInTheDocument();
  });

  it("loads older revisions in pages as the list scrolls", async () => {
    const { log } = await openHistory();
    expect(log).toHaveBeenCalledTimes(1);
    expect(log).toHaveBeenLastCalledWith(WS, null, null, 100);
    const list = screen.getByRole("listbox", { name: "Revisions" });
    // Only the rows near the top are rendered.
    expect(within(list).getAllByRole("option").length).toBeLessThan(40);
    expect(within(list).queryByRole("option", { name: /^r1300 / })).toBeNull();

    list.scrollTop = 90 * 40;
    fireEvent.scroll(list);
    await waitFor(() => expect(log).toHaveBeenCalledTimes(2));
    expect(log).toHaveBeenLastCalledWith(WS, null, 1383, 100);

    list.scrollTop = 180 * 40;
    fireEvent.scroll(list);
    expect(await within(list).findByRole("option", { name: /^r1300 / })).toBeInTheDocument();
    expect(within(list).queryByRole("option", { name: /^r1482 / })).toBeNull();
  });

  it("shows the imported start of the depot", async () => {
    const { log, user } = await openHistory();
    const list = screen.getByRole("listbox", { name: "Revisions" });
    // 1482 revisions in pages of 100.
    for (let page = 2; page <= 15; page++) {
      list.scrollTop = page * 100 * 40;
      fireEvent.scroll(list);
      await waitFor(() => expect(log).toHaveBeenCalledTimes(page));
    }
    list.scrollTop = 1470 * 40;
    fireEvent.scroll(list);
    const oldest = await within(list).findByRole("option", { name: /^r1 / });
    expect(within(oldest).getByText("imported from git")).toBeInTheDocument();
    await user.click(oldest);
    const details = screen.getByRole("complementary", { name: "Revision" });
    expect(await within(details).findByText("Imported")).toBeInTheDocument();
  });

  it("shows a revision's changed files and opens file history and blame from it", async () => {
    const { log, user } = await openHistory();
    await user.click(screen.getByRole("option", { name: /^r1478 / }));
    const details = screen.getByRole("complementary", { name: "Revision" });
    expect(await within(details).findByText("feature/harbor-night r1461..r1474")).toBeInTheDocument();
    const files = within(details).getByRole("list", { name: "Changed files" });
    const firstFile = within(files).getAllByRole("listitem")[0];
    const path = within(firstFile!).getByTitle(/\//).getAttribute("title") ?? "";

    await user.click(within(details).getByRole("button", { name: `History of ${path}` }));
    expect(await screen.findByRole("heading", { name: /^History of / })).toHaveTextContent(path.split("/").pop()!);
    expect(log).toHaveBeenLastCalledWith(WS, path, null, 100);
    const list = screen.getByRole("listbox", { name: "Revisions" });
    await within(list).findByRole("option", { name: /^r1478 / });

    await user.click(screen.getByRole("button", { name: "Branch history" }));
    expect(await screen.findByRole("heading", { name: "History of main" })).toBeInTheDocument();
  });

  it("blames a file and opens the revision that last changed a line", async () => {
    const { user } = await openHistory();
    await user.click(screen.getByRole("option", { name: /^r1482 / }));
    const details = screen.getByRole("complementary", { name: "Revision" });
    await user.click(await within(details).findByRole("button", { name: "Blame Source/Skyfall/Boat.cpp" }));
    const view = await screen.findByRole("region", { name: "Blame" });
    expect(await within(view).findByText(/ABoat::Tick/)).toBeInTheDocument();
    expect(within(view).getByText(/at r1482/)).toBeInTheDocument();
    expect(within(view).getAllByRole("button", { name: /^r\d+/ }).length).toBeGreaterThan(3);
    await user.click(within(view).getAllByRole("button", { name: /^r1477/ })[0]!);
    const rev = screen.getByRole("complementary", { name: "Revision" });
    expect(await within(rev).findByRole("heading", { name: /r1477/ })).toBeInTheDocument();
    expect(await screen.findByRole("option", { name: /^r1477 / })).toHaveAttribute("aria-selected", "true");
  });

  it("opens a pending file's history and blame from the Changes view", async () => {
    setBackend(createMockBackend({ stepMs: 0 }));
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByText(WS));
    const pending = await screen.findByRole("region", { name: "Pending changes" });
    await user.click(within(pending).getByText("Boat.cpp"));
    await user.type(screen.getByLabelText("Commit message"), "Half written");
    const details = screen.getByRole("complementary", { name: "Details" });
    await user.click(within(details).getByRole("button", { name: "History" }));
    expect(await screen.findByRole("heading", { name: /^History of Boat\.cpp/ })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Pending changes" })).toBeNull();

    // Back in Changes, the message typed before is kept.
    await user.click(screen.getByRole("button", { name: /^Changes/ }));
    expect(screen.getByLabelText("Commit message")).toHaveValue("Half written");
    await user.click(within(screen.getByRole("complementary", { name: "Details" })).getByRole("button", { name: "Blame" }));
    expect(await screen.findByRole("region", { name: "Blame" })).toBeInTheDocument();
  });

  it("moves the selection with the arrow keys", async () => {
    const { user } = await openHistory();
    const list = screen.getByRole("listbox", { name: "Revisions" });
    list.focus();
    await user.keyboard("{ArrowDown}{ArrowDown}");
    expect(within(list).getByRole("option", { name: /^r1480 / })).toHaveAttribute("aria-selected", "true");
    await user.keyboard("{ArrowUp}");
    expect(within(list).getByRole("option", { name: /^r1481 / })).toHaveAttribute("aria-selected", "true");
  });
});
