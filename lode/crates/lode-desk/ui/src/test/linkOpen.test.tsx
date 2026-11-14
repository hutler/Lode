import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { setBackend } from "../api";
import { App } from "../App";
import { createMockBackend, type MockOptions } from "../mock";

const WS = "D:\\Work\\Skyfall";
const RELEASE = "D:\\Work\\Skyfall-release";
const PIPELINE = "D:\\Tools\\pipeline";

function start(opts: MockOptions = {}) {
  const b = createMockBackend({ stepMs: 0, ...opts });
  setBackend(b);
  const user = userEvent.setup();
  render(<App />);
  return { b, user };
}

const tabs = () => screen.getByRole("navigation", { name: "Open workspaces" });

describe("copying links", () => {
  it("copies links to a revision, to a file in it and in its history, and to a shelf", async () => {
    const { user } = start();
    await user.click(await screen.findByText(WS));
    await user.click(await screen.findByRole("button", { name: "History" }));
    await user.click(await screen.findByRole("option", { name: /^r1482 / }));
    const details = screen.getByRole("complementary", { name: "Revision" });

    await user.click(await within(details).findByRole("button", { name: "Copy link to r1482" }));
    expect(await navigator.clipboard.readText()).toBe("lode://lode.example.com/skyfall/r1482");
    expect(within(details).getByRole("button", { name: "Copy link to r1482" })).toHaveTextContent("Copied");

    await user.click(within(details).getByRole("button", { name: "Copy link to Source/Skyfall/Boat.cpp at r1482" }));
    expect(await navigator.clipboard.readText()).toBe("lode://lode.example.com/skyfall/Source/Skyfall/Boat.cpp@1482");

    await user.click(within(details).getByRole("button", { name: "History of Source/Skyfall/Boat.cpp" }));
    await screen.findByRole("heading", { name: /^History of Boat.cpp/ });
    const fileDetails = screen.getByRole("complementary", { name: "Revision" });
    const fileLink = await within(fileDetails).findByRole("button", {
      name: /^Copy link to Source\/Skyfall\/Boat.cpp at r\d+$/,
    });
    const rev = /r(\d+)$/.exec(fileLink.getAttribute("aria-label") ?? "")?.[1];
    await user.click(fileLink);
    expect(await navigator.clipboard.readText()).toBe(`lode://lode.example.com/skyfall/Source/Skyfall/Boat.cpp@${rev}`);

    await user.click(screen.getByRole("button", { name: "Shelves" }));
    const shelf = await screen.findByRole("article", { name: "Shelf 44" });
    await user.click(within(shelf).getByRole("button", { name: "Copy link to shelf 44" }));
    expect(await navigator.clipboard.readText()).toBe("lode://lode.example.com/skyfall/shelf/44");
  });
});

describe("opening links", () => {
  it("opens a revision link the app was started with in the one workspace of that depot", async () => {
    start({ launchLink: "lode://lode.example.com/pipeline/r300" });
    expect(await within(tabs()).findByRole("button", { name: /^pipeline/ })).toHaveAttribute("aria-current", "page");
    const details = await screen.findByRole("complementary", { name: "Revision" });
    expect(await within(details).findByRole("heading", { name: /^r300/ })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "History of main" })).toBeInTheDocument();
  });

  it("asks which workspace when several fit, putting the link's branch first", async () => {
    const { b, user } = start();
    await screen.findByText(WS);
    b.handLink("lode://lode.example.com/skyfall/Docs/Release-1.2.md?branch=release-1.2");
    const ask = await screen.findByRole("dialog", { name: "Open the link in which workspace?" });
    const choices = within(ask).getAllByRole("button", { name: /^Open in / });
    expect(choices.map((c) => c.getAttribute("aria-label"))).toEqual([`Open in ${RELEASE}`, `Open in ${WS}`]);
    await user.click(choices[0]!);
    expect(await screen.findByRole("heading", { name: /^History of Release-1.2.md/ })).toBeInTheDocument();
    expect(within(tabs()).getByRole("button", { name: /^skyfall/ })).toHaveTextContent("release-1.2");
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("says when a branch link is shown in a workspace on another branch", async () => {
    const { b, user } = start();
    await screen.findByText(WS);
    b.handLink("lode://lode.example.com/skyfall/Docs/Release-1.2.md?branch=release-1.2");
    const ask = await screen.findByRole("dialog", { name: "Open the link in which workspace?" });
    await user.click(within(ask).getByRole("button", { name: `Open in ${WS}` }));
    expect(await screen.findByRole("status")).toHaveTextContent(
      "The link is for Docs/Release-1.2.md on release-1.2, and this workspace is on main",
    );
  });

  it("opens a file link at its revision in an open workspace, and a shelf link even when the filter hides it", async () => {
    const { b, user } = start({ recent: [] });
    await user.click(await screen.findByRole("button", { name: /Check out/ }));
    const form = screen.getByRole("dialog", { name: "Check out a depot" });
    await user.clear(within(form).getByRole("textbox", { name: /Depot/ }));
    await user.type(within(form).getByRole("textbox", { name: /Depot/ }), "https://lode.example.com/skyfall");
    await user.type(within(form).getByRole("textbox", { name: /Folder/ }), "D:\\Work\\Fresh");
    await user.click(within(form).getByRole("button", { name: "Check out" }));
    await within(tabs()).findByRole("button", { name: /^skyfall/ });
    await user.click(screen.getByRole("button", { name: "Home" }));

    b.handLink("lode://Lode.Example.com/skyfall/Source/Skyfall/Boat.cpp@1482");
    expect(await screen.findByRole("heading", { name: /^History of Boat.cpp/ })).toBeInTheDocument();
    const details = screen.getByRole("complementary", { name: "Revision" });
    expect(await within(details).findByRole("heading", { name: /^r1482/ })).toBeInTheDocument();

    b.handLink("lode://lode.example.com/skyfall/shelf/30");
    const shelf = await screen.findByRole("article", { name: "Shelf 30" });
    expect(shelf).toHaveTextContent("Harbor lights pass");
    expect(screen.getByRole("combobox", { name: "Shelf state" })).toHaveValue("open");
  });

  it("offers a checkout when no workspace fits, then shows the link in it", async () => {
    const { b, user } = start({
      recent: [{ path: PIPELINE, depot: "pipeline", server: "https://lode.example.com", branch: "main" }],
    });
    const checkout = vi.spyOn(b, "checkout");
    await screen.findByText(PIPELINE);
    b.handLink("lode://lode.example.com/skyfall/shelf/44");
    const none = await screen.findByRole("dialog", { name: "No workspace for this link" });
    expect(none).toHaveTextContent("The link shows shelf 44 in skyfall on lode.example.com");
    await user.click(within(none).getByRole("button", { name: "Check out skyfall…" }));
    const form = screen.getByRole("dialog", { name: "Check out a depot" });
    expect(within(form).getByRole("textbox", { name: /Depot/ })).toHaveValue("https://lode.example.com/skyfall");
    await user.click(within(form).getByRole("button", { name: "Browse…" }));
    await user.click(within(form).getByRole("button", { name: "Check out" }));
    await waitFor(() =>
      expect(checkout).toHaveBeenCalledWith("https://lode.example.com/skyfall", "D:\\Work\\Skyfall-new", "main"),
    );
    expect(await screen.findByRole("article", { name: "Shelf 44" })).toBeInTheDocument();
  });

  it("refuses a link that is not exactly a lode link, and opens nothing", async () => {
    const { b, user } = start({ launchLink: "lode://lode.example.com/skyfall/Docs/../secret.txt@3" });
    const open = vi.spyOn(b, "openWorkspace");
    const refused = await screen.findByRole("dialog", { name: "This link cannot be opened" });
    expect(refused).toHaveTextContent("A path has an empty, `.` or `..` part. Nothing was opened.");
    await user.click(within(refused).getByRole("button", { name: "OK" }));

    b.handLink("lode://user@lode.example.com/skyfall/r1");
    expect(await screen.findByRole("dialog", { name: "This link cannot be opened" })).toHaveTextContent(
      "Links do not carry user names",
    );
    expect(open).not.toHaveBeenCalled();
    expect(within(tabs()).getAllByRole("button")).toHaveLength(1);
  });
});
