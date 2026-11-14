import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";

import { setBackend, type FileDiff } from "../api";
import { App } from "../App";
import { FileDiffBody } from "../components/DiffView";
import { PLAIN_LIMIT, parsePatch, splitRows, wordDiff } from "../diff";
import { createMockBackend } from "../mock";

const WS = "D:\\Work\\Skyfall";

const PATCH = `diff --lode a/x.cpp b/x.cpp
--- a/x.cpp
+++ b/x.cpp
@@ -3,5 +3,6 @@
 int a = 1;
-int speed = 10;
-int unused = 0;
+int speed = 12;
 int b = 2;
+int c = 3;
+int d = 4;
@@ -20,2 +21,2 @@
-return a;
+return b;
\\ No newline at end of file
`;

function fileDiff(extra: Partial<FileDiff>): FileDiff {
  return {
    path: "x.cpp",
    status: "modified",
    old_path: "x.cpp",
    binary: false,
    old_size: 100,
    new_size: 110,
    old_mode: "100644",
    new_mode: "100644",
    insertions: 1,
    deletions: 1,
    ...extra,
  };
}

describe("patch parsing", () => {
  it("numbers the lines of each hunk and skips the header and markers", () => {
    const hunks = parsePatch(PATCH);
    expect(hunks).toHaveLength(2);
    expect(hunks[0]!.lines.map((l) => [l.kind[0], l.old, l.new])).toEqual([
      ["c", 3, 3],
      ["d", 4, null],
      ["d", 5, null],
      ["a", null, 4],
      ["c", 6, 5],
      ["a", null, 6],
      ["a", null, 7],
    ]);
    expect(hunks[1]!.lines.map((l) => l.text)).toEqual(["return a;", "return b;"]);
  });

  it("reads an empty line inside a hunk as an empty context line", () => {
    const [h] = parsePatch("@@ -1,3 +1,3 @@\n a\n\n-b\n+c\n");
    expect(h!.lines.map((l) => [l.kind, l.text, l.old, l.new])).toEqual([
      ["context", "a", 1, 1],
      ["context", "", 2, 2],
      ["delete", "b", 3, null],
      ["add", "c", null, 3],
    ]);
  });

  it("pairs each deleted line with the most alike added line", () => {
    const [h] = parsePatch(
      "@@ -1,2 +1,3 @@\n-const float Wave = Sample(Location());\n-Roll = 0.2f;\n+const FVector Here = Location();\n+const float Wave = Sample(Here);\n+Roll = 0.35f;\n",
    );
    expect(splitRows(h!).map((r) => [r.left?.text ?? null, r.right?.text ?? null])).toEqual([
      [null, "const FVector Here = Location();"],
      ["const float Wave = Sample(Location());", "const float Wave = Sample(Here);"],
      ["Roll = 0.2f;", "Roll = 0.35f;"],
    ]);
  });

  it("marks only the words that changed inside a changed line", () => {
    const [left, right] = wordDiff("int speed = 10;", "int speed = 12;");
    expect(left.filter((s) => s.changed).map((s) => s.text)).toEqual(["10"]);
    expect(right.filter((s) => s.changed).map((s) => s.text)).toEqual(["12"]);
    expect(left.map((s) => s.text).join("")).toBe("int speed = 10;");
    const [h] = parsePatch(PATCH);
    const speed = h!.lines.find((l) => l.text === "int speed = 12;");
    expect(speed?.words?.filter((s) => s.changed).map((s) => s.text)).toEqual(["12"]);
    // A deleted line with no added line next to it gets no word marks.
    expect(h!.lines.find((l) => l.text === "int unused = 0;")?.words).toBeUndefined();
  });

  it("lays out side by side with deleted lines next to added ones", () => {
    const rows = splitRows(parsePatch(PATCH)[0]!);
    expect(rows.map((r) => [r.left?.text ?? null, r.right?.text ?? null])).toEqual([
      ["int a = 1;", "int a = 1;"],
      ["int speed = 10;", "int speed = 12;"],
      ["int unused = 0;", null],
      ["int b = 2;", "int b = 2;"],
      [null, "int c = 3;"],
      [null, "int d = 4;"],
    ]);
  });
});

describe("diff content", () => {
  it("shows a binary file's sizes and kind instead of a diff", () => {
    render(
      <FileDiffBody
        diff={fileDiff({ path: "Maps/Harbor.umap", binary: true, old_size: 2_412_544, new_size: 2_430_976 })}
        layout="unified"
      />,
    );
    const note = screen.getByRole("note");
    expect(note).toHaveTextContent("Binary file");
    expect(note).toHaveTextContent(".umap");
    expect(note).toHaveTextContent("2.4 MB");
    expect(note).toHaveTextContent("2.4 MB");
    expect(screen.queryByRole("table")).toBeNull();
  });

  it("says when a file is too large for Lode to compare as text", () => {
    render(<FileDiffBody diff={fileDiff({ binary: true, old_size: 20_000_000, new_size: 21_000_000 })} layout="unified" />);
    expect(screen.getByRole("note")).toHaveTextContent("Too large to compare as text");
  });

  it("falls back to plain text for a very large patch and says so", () => {
    const line = "+" + "x".repeat(99) + "\n";
    const patch = "@@ -0,0 +1,6000 @@\n" + line.repeat(Math.ceil(PLAIN_LIMIT / line.length) + 10);
    render(<FileDiffBody diff={fileDiff({ patch })} layout="split" />);
    expect(screen.getByRole("note")).toHaveTextContent(/large .* plain text/);
    expect(screen.queryByRole("table")).toBeNull();
  });
});

describe("diff view", () => {
  beforeEach(() => {
    localStorage.clear();
    setBackend(createMockBackend({ stepMs: 0 }));
  });

  async function openBoatDiff() {
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByText(WS));
    const pending = await screen.findByRole("region", { name: "Pending changes" });
    await user.click(within(pending).getByText("Boat.cpp"));
    await user.click(screen.getByRole("button", { name: "Show diff" }));
    return user;
  }

  it("shows a working file's diff in both layouts and remembers the choice", async () => {
    const user = await openBoatDiff();
    const table = await screen.findByRole("table", { name: "Unified diff" });
    expect(within(table).getAllByText(/^@@ /)).toHaveLength(1);
    const marks = table.querySelectorAll("mark.word");
    expect([...marks].map((m) => m.textContent)).toContain("35f");
    expect(screen.getByText("+7")).toBeInTheDocument();
    expect(screen.getByText("−4")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Side by side" }));
    const split = screen.getByRole("table", { name: "Side-by-side diff" });
    expect(within(split).getAllByText(/Roll = FMath/)).toHaveLength(2);
    expect(localStorage.getItem("lode-desk.diff-layout")).toBe("split");

    await user.click(screen.getByRole("button", { name: /Changes$/ }));
    expect(await screen.findByRole("region", { name: "Pending changes" })).toBeInTheDocument();
  });

  it("shows a binary working file by size", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByText(WS));
    const pending = await screen.findByRole("region", { name: "Pending changes" });
    await user.dblClick(within(pending).getByText("Harbor.umap"));
    expect(await screen.findByRole("note")).toHaveTextContent("Binary file");
  });

  it("opens a file as a revision changed it from the history", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByText(WS));
    await user.click(await screen.findByRole("button", { name: "History" }));
    await user.click(await screen.findByRole("option", { name: /^r1478 / }));
    const details = screen.getByRole("complementary", { name: "Revision" });
    await user.click(await within(details).findByRole("button", { name: "Diff of Config/DefaultEngine.ini" }));
    const view = await screen.findByRole("region", { name: "Diff" });
    expect(within(view).getByText(/in r1478/)).toBeInTheDocument();
    expect(await within(view).findByRole("table", { name: "Unified diff" })).toHaveTextContent("r.Streaming.PoolSize");

    await user.click(within(view).getByRole("button", { name: /r1478$/ }));
    expect(await screen.findByRole("option", { name: /^r1478 / })).toHaveAttribute("aria-selected", "true");
  });
});
