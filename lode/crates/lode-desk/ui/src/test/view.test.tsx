import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setBackend } from "../api";
import { App } from "../App";
import { createMockBackend, type MockOptions } from "../mock";
import { folderState, globMatch, inView, parsePatterns, tickFolder } from "../viewPatterns";

const WS = "D:\\Work\\Skyfall";

async function openView(opts: MockOptions = {}) {
  const b = createMockBackend({ stepMs: 0, ...opts });
  setBackend(b);
  const user = userEvent.setup();
  render(<App />);
  await user.click(await screen.findByText(WS));
  await user.click(await screen.findByRole("button", { name: "Workspace" }));
  const view = await screen.findByRole("region", { name: "Workspace view" });
  await within(view).findByRole("tree", { name: "Depot folders" });
  return { b, user, view };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("view patterns", () => {
  it("match like the server: * within a folder, ** across, braces and classes", () => {
    expect(globMatch("Content/**", "Content/Maps/Harbor.umap")).toBe(true);
    expect(globMatch("Content/*", "Content/Maps/Harbor.umap")).toBe(false);
    expect(globMatch("*.txt", "notes.txt")).toBe(true);
    expect(globMatch("*.txt", "Docs/notes.txt")).toBe(false);
    expect(globMatch("**/*.txt", "Docs/notes.txt")).toBe(true);
    expect(globMatch("**/*.txt", "notes.txt")).toBe(true);
    expect(globMatch("Content/{Maps,UI}/**", "Content/UI/a.png")).toBe(true);
    expect(globMatch("Content/{Maps,UI}/**", "Content/Props/a.png")).toBe(false);
    expect(globMatch("v[0-9].md", "v2.md")).toBe(true);
    expect(globMatch("a.b", "aXb")).toBe(false);
    expect(inView({ include: [], exclude: [] }, "x")).toBe(true);
    expect(inView({ include: ["a/**"], exclude: ["a/b/**"] }, "a/b/c")).toBe(false);
    expect(parsePatterns(" a/**\n\n b \r\n")).toEqual(["a/**", "b"]);
  });

  it("tell whether a folder is in, partly in, or kept out from above", () => {
    const spec = { include: [], exclude: ["Content/Cinematics/**", "Tools/**"] };
    expect(folderState(spec, "Content")).toEqual({ on: true, mixed: true, lockedBy: null });
    expect(folderState(spec, "Content/Cinematics")).toEqual({ on: false, mixed: false, lockedBy: null });
    expect(folderState(spec, "Tools/Pipeline")).toEqual({ on: false, mixed: false, lockedBy: "Tools/**" });
    expect(folderState({ include: ["Content/Maps/**"], exclude: [] }, "Content")).toEqual({
      on: false,
      mixed: true,
      lockedBy: null,
    });
  });

  it("turn ticks into patterns", () => {
    const all = { include: [], exclude: [] };
    expect(tickFolder(all, "Docs", false)).toEqual({ include: [], exclude: ["Docs/**"] });
    expect(tickFolder({ include: [], exclude: ["Docs/**"] }, "Docs", true)).toEqual(all);
    const some = { include: ["Source/**"], exclude: [] };
    expect(tickFolder(some, "Docs", true)).toEqual({ include: ["Source/**", "Docs/**"], exclude: [] });
    expect(tickFolder({ include: ["Source/**", "Docs/**"], exclude: [] }, "Docs", false)).toEqual(some);
    // Taking out the only include would bring everything in, so it is excluded instead.
    expect(tickFolder(some, "Source", false)).toEqual({ include: ["Source/**"], exclude: ["Source/**"] });
    // Deciding a whole folder drops the patterns inside it.
    expect(tickFolder({ include: [], exclude: ["Content/Cinematics/**"] }, "Content", false)).toEqual({
      include: [],
      exclude: ["Content/**"],
    });
  });
});

describe("the view editor", () => {
  it("shows what the view holds and ticks folders into patterns", async () => {
    const { user, view } = await openView();
    const facts = within(view).getByLabelText("What the view holds");
    expect(facts).toHaveTextContent("In the view at r1482");
    expect(facts).toHaveTextContent("8,188 files, 5.7 GB");
    const exclude = within(view).getByRole("textbox", { name: "Exclude patterns" });
    expect(exclude).toHaveValue("Content/Cinematics/**");

    const content = within(view).getByRole("checkbox", { name: "Include Content" });
    expect(content).toBeChecked();
    expect((content as HTMLInputElement).indeterminate).toBe(true);
    await user.click(within(view).getByRole("button", { name: "Open Content" }));
    const cinematics = await within(view).findByRole("checkbox", { name: "Include Content/Cinematics" });
    expect(cinematics).not.toBeChecked();

    await user.click(cinematics);
    expect(exclude).toHaveValue("");
    await user.click(within(view).getByRole("checkbox", { name: "Include Docs" }));
    expect(exclude).toHaveValue("Docs/**");
    await user.click(within(view).getByRole("button", { name: "Reset" }));
    expect(exclude).toHaveValue("Content/Cinematics/**");
    expect(within(view).getByRole("button", { name: "Preview change" })).toBeDisabled();
  });

  it("follows patterns typed as text", async () => {
    const { user, view } = await openView();
    await user.type(within(view).getByRole("textbox", { name: "Include patterns" }), "Content/Maps/**");
    const content = within(view).getByRole("checkbox", { name: "Include Content" });
    expect(content).not.toBeChecked();
    expect((content as HTMLInputElement).indeterminate).toBe(true);
    expect(within(view).getByRole("checkbox", { name: "Include Config" })).not.toBeChecked();
    await user.click(within(view).getByRole("button", { name: "Open Content" }));
    expect(await within(view).findByRole("checkbox", { name: "Include Content/Maps" })).toBeChecked();
  });

  it("previews what is fetched and removed, then applies it", async () => {
    const { b, user, view } = await openView();
    const apply = vi.spyOn(b, "viewApply");
    await user.click(within(view).getByRole("button", { name: "Open Content" }));
    await user.click(await within(view).findByRole("checkbox", { name: "Include Content/Cinematics" }));
    await user.click(within(view).getByRole("checkbox", { name: "Include Docs" }));
    await user.click(within(view).getByRole("button", { name: "Preview change" }));
    expect(apply).toHaveBeenLastCalledWith(WS, [], ["Docs/**"], false, true);
    const preview = await within(view).findByRole("region", { name: "Preview" });
    expect(preview).toHaveTextContent("Fetch 412 files (3.2 GB); remove 58 files from disk.");
    await user.click(within(preview).getByText("Show files"));
    expect(within(preview).getByText("+ Content/Cinematics/Cinematics_01.uasset")).toBeInTheDocument();
    expect(within(preview).getByText("− Docs/Docs_01.md")).toBeInTheDocument();

    await user.click(within(view).getByRole("button", { name: "Apply" }));
    expect(apply).toHaveBeenLastCalledWith(WS, [], ["Docs/**"], false, false);
    expect(await within(view).findByRole("status")).toHaveTextContent("View updated: 412 files fetched, 58 removed.");
    await waitFor(() => expect(view).toHaveTextContent("8,542 files, 8.9 GB"));
    expect(within(view).getByRole("textbox", { name: "Exclude patterns" })).toHaveValue("Docs/**");
  });

  it("names changed files that would leave, and drops them only after a second yes", async () => {
    const { b, user, view } = await openView();
    const apply = vi.spyOn(b, "viewApply");
    await user.click(within(view).getByRole("checkbox", { name: "Include Source" }));
    await user.click(within(view).getByRole("button", { name: "Preview change" }));
    const kept = await within(view).findByRole("note", { name: "Files with local changes" });
    expect(kept).toHaveTextContent("Source/Skyfall/Boat.cpp");
    expect(kept).toHaveTextContent("Source/Skyfall/Sail.cpp");
    expect(within(view).queryByRole("button", { name: "Apply" })).toBeNull();

    await user.click(within(view).getByRole("button", { name: "Drop them anyway…" }));
    const ask = screen.getByRole("dialog", { name: "Drop local changes?" });
    expect(ask).toHaveTextContent("The changes to 2 files are lost");
    await user.click(within(ask).getByRole("button", { name: "Cancel" }));
    expect(apply).not.toHaveBeenCalledWith(WS, [], expect.anything(), true, false);

    await user.click(within(view).getByRole("button", { name: "Drop them anyway…" }));
    await user.click(
      within(screen.getByRole("dialog", { name: "Drop local changes?" })).getByRole("button", {
        name: "Drop and apply",
      }),
    );
    expect(apply).toHaveBeenLastCalledWith(WS, [], ["Content/Cinematics/**", "Source/**"], true, false);
    expect(await within(view).findByRole("status")).toHaveTextContent("1,210 removed");
  });

  it("keeps applying while the rest of the app is used", async () => {
    const { user, view } = await openView({ stepMs: 30 });
    await user.click(within(view).getByRole("checkbox", { name: "Include Docs" }));
    await user.click(within(view).getByRole("button", { name: "Preview change" }));
    await user.click(await within(view).findByRole("button", { name: "Apply" }));
    expect(within(view).getByText(/You can keep working meanwhile/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /^Changes/ }));
    expect(screen.getByRole("button", { name: /^Workspace/ })).toHaveTextContent("…");
    expect(await screen.findByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" })).toBeVisible();
    await waitFor(() => expect(screen.getByRole("button", { name: /^Workspace/ })).not.toHaveTextContent("…"), {
      timeout: 2000,
    });
    await user.click(screen.getByRole("button", { name: /^Workspace/ }));
    expect(within(view).getByRole("status")).toHaveTextContent("View updated");
  });
});
