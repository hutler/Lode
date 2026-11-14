import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setBackend, type CheckInfo } from "../api";
import { App } from "../App";
import { CHECK_PAGE, FRESH_MS, summarize } from "../checks";
import { ROW } from "../components/HistoryView";
import { createMockBackend, type MockOptions } from "../mock";

const WS = "D:\\Work\\Skyfall";

function c(name: string, state: CheckInfo["state"]): CheckInfo {
  return { name, state, reporter: { name: "ci", kind: "user" }, updated_at: "2026-11-14T08:50:00Z" };
}

async function openHistory(opts: MockOptions = {}) {
  const b = createMockBackend({ stepMs: 0, ...opts });
  const checks = vi.spyOn(b, "checks");
  setBackend(b);
  const user = userEvent.setup();
  render(<App />);
  await user.click(await screen.findByText(WS));
  await user.click(await screen.findByRole("button", { name: "History" }));
  await screen.findAllByRole("option", { name: /^r\d+ / });
  return { b, checks, user };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("the check badge", () => {
  it("shows the worst state with how many are in it", () => {
    expect(summarize([])).toBeNull();
    expect(summarize([c("a", "success"), c("b", "success")])).toEqual({ state: "success", count: 2, total: 2 });
    expect(summarize([c("a", "success"), c("b", "pending")])).toEqual({ state: "pending", count: 1, total: 2 });
    expect(summarize([c("a", "cancelled"), c("b", "pending")])).toEqual({ state: "cancelled", count: 1, total: 2 });
    expect(summarize([c("a", "error"), c("b", "cancelled"), c("c", "error")])).toEqual({
      state: "error",
      count: 2,
      total: 3,
    });
    expect(summarize([c("a", "error"), c("b", "failure"), c("c", "success")])).toEqual({
      state: "failure",
      count: 1,
      total: 3,
    });
  });
});

describe("checks in History", () => {
  it("badges each row and lists the selected revision's checks with links", async () => {
    const { b, user } = await openHistory();
    const open = vi.spyOn(b, "openLink");
    const row = (rev: number) => screen.getByRole("option", { name: new RegExp(`^r${rev} `) });
    expect(await within(row(1482)).findByRole("img", { name: "1 of 2 checks pending" })).toBeInTheDocument();
    expect(within(row(1481)).getByRole("img", { name: "1 of 2 checks failed" })).toBeInTheDocument();
    expect(within(row(1480)).getByRole("img", { name: "2 checks passed" })).toBeInTheDocument();
    // r1477 has no checks, so no badge.
    expect(within(row(1477)).queryByRole("img")).toBeNull();

    await user.click(row(1481));
    const details = screen.getByRole("complementary", { name: "Revision" });
    const list = await within(details).findByRole("list", { name: "Checks" });
    const items = within(list).getAllByRole("listitem");
    expect(items.map((i) => i.textContent)).toEqual([
      expect.stringContaining("passedci/buildBuilt in 6 minci, "),
      expect.stringContaining("failedci/test3 tests failedci, "),
    ]);
    await user.click(within(list).getByRole("button", { name: "ci/test" }));
    expect(open).toHaveBeenCalledWith("https://ci.example.com/skyfall/r1481/ci-test");
  });

  it("loads checks for the rows on screen only, a page at a time", async () => {
    const { checks } = await openHistory();
    await waitFor(() => expect(checks).toHaveBeenCalled());
    const asked = checks.mock.calls.flatMap((call) => call[1]);
    expect(checks.mock.calls.every((call) => call[1].length <= CHECK_PAGE)).toBe(true);
    // A hundred revisions are loaded; only the rendered ones (the window and a few around it) ask.
    expect(asked.length).toBeLessThan(40);
    expect(asked).toContain("r1482");
    expect(asked).not.toContain("r1400");
  });

  it("asks again for a row that comes back into view after a minute", async () => {
    const { checks } = await openHistory();
    await waitFor(() => expect(checks).toHaveBeenCalled());
    const list = screen.getByRole("listbox", { name: "Revisions" });
    const scrollTo = (top: number) =>
      act(() => {
        list.scrollTop = top;
        fireEvent.scroll(list);
      });
    const askedFor = (target: string) => checks.mock.calls.filter((call) => call[1].includes(target)).length;
    expect(askedFor("r1482")).toBe(1);

    scrollTo(60 * ROW);
    await waitFor(() => expect(askedFor("r1420")).toBe(1));
    // Back within the minute: nothing is asked again.
    scrollTo(0);
    await new Promise((r) => setTimeout(r, 20));
    expect(askedFor("r1482")).toBe(1);

    // A minute later it is.
    const later = Date.now() + FRESH_MS + 1_000;
    vi.spyOn(Date, "now").mockReturnValue(later);
    scrollTo(60 * ROW);
    scrollTo(0);
    await waitFor(() => expect(askedFor("r1482")).toBe(2));
  });

  it("shows no badges and no error on a server without checks", async () => {
    const { checks } = await openHistory({ noChecks: true });
    await waitFor(() => expect(checks).toHaveBeenCalledTimes(1));
    expect(screen.queryAllByRole("img")).toHaveLength(0);
    expect(screen.queryByRole("alert")).toBeNull();
    // Scrolling does not ask again.
    const list = screen.getByRole("listbox", { name: "Revisions" });
    act(() => {
      list.scrollTop = 60 * ROW;
      fireEvent.scroll(list);
    });
    await new Promise((r) => setTimeout(r, 20));
    expect(checks).toHaveBeenCalledTimes(1);
  });
});

describe("checks on shelves", () => {
  it("badges each shelf version and lists the shown shelf's checks", async () => {
    const b = createMockBackend({ stepMs: 0 });
    setBackend(b);
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByText(WS));
    await user.click(screen.getByRole("button", { name: "Shelves" }));
    const view = await screen.findByRole("region", { name: "Shelves" });
    const item = async (id: number) => within(view).findByRole("option", { name: new RegExp(`^Shelf ${id}:`) });
    expect(await within(await item(44)).findByRole("img", { name: "1 of 1 check failed" })).toBeInTheDocument();
    expect(within(await item(42)).getByRole("img", { name: "1 of 2 checks pending" })).toBeInTheDocument();
    expect(within(await item(38)).getByRole("img", { name: "2 checks passed" })).toBeInTheDocument();
    const shelf = await within(view).findByRole("article", { name: "Shelf 44" });
    const list = await within(shelf).findByRole("list", { name: "Checks" });
    expect(list).toHaveTextContent("2 tests failed");
  });
});
