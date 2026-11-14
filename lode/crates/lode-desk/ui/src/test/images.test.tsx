import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { decodeImageSide, setBackend, type StatusFile } from "../api";
import { App } from "../App";
import { createMockBackend } from "../mock";

const WS = "D:\\Work\\Skyfall";

function file(path: string, status: StatusFile["status"] = "modified"): StatusFile {
  return { path, status, copy_from: null, moved: false, changelist: null, locked: false, needs_lock: false, lock_required: false };
}

/** Reports a size for an image, as a browser does once it has decoded it. */
function decoded(img: HTMLElement, w: number, h: number) {
  Object.defineProperty(img, "naturalWidth", { value: w, configurable: true });
  Object.defineProperty(img, "naturalHeight", { value: h, configurable: true });
  fireEvent.load(img);
}

async function openImage(path: string, status: StatusFile["status"] = "modified") {
  const b = createMockBackend({ stepMs: 0 });
  setBackend(b);
  const user = userEvent.setup();
  render(<App />);
  await user.click(await screen.findByText(WS));
  const pending = await screen.findByRole("region", { name: "Pending changes" });
  b.touch(WS, file(path, status));
  const name = path.slice(path.lastIndexOf("/") + 1);
  await user.dblClick(await within(pending).findByText(name));
  const view = await screen.findByRole("region", { name: "Diff" });
  return { b, user, view };
}

beforeEach(() => {
  localStorage.clear();
});

describe("image diff", () => {
  it("shows a working image side by side with each side's dimensions and size", async () => {
    const { b, view } = await openImage("Content/UI/HarborMap.png");
    const side = vi.spyOn(b, "imageSide");
    const before = await within(view).findByRole("img", { name: "Before" });
    const after = within(view).getByRole("img", { name: "After" });
    decoded(before, 640, 400);
    decoded(after, 1280, 800);
    const captions = within(view).getAllByText(/^(Before|After)$/).map((c) => c.closest("figcaption")!.textContent);
    expect(captions).toEqual(["Before640 × 4001.8 MB", "After1280 × 8001.9 MB"]);
    // The text layout switch is not offered for images.
    expect(within(view).queryByRole("group", { name: "Layout" })).toBeNull();
    expect(side).not.toHaveBeenCalled();
  });

  it("swipes between the two with a slider and remembers the mode", async () => {
    const { user, view } = await openImage("Content/UI/HarborMap.png");
    await within(view).findByRole("img", { name: "Before" });
    const modes = within(view).getByRole("group", { name: "Image comparison" });
    await user.click(within(modes).getByRole("button", { name: "Swipe" }));
    expect(localStorage.getItem("lode-desk.image-mode")).toBe("swipe");
    const after = within(view).getByRole("img", { name: "After" });
    expect(after.style.clipPath).toMatch(/^inset\(0(px)? 0(px)? 0(px)? 50%\)$/);
    fireEvent.change(within(view).getByRole("slider", { name: "Swipe position" }), { target: { value: "20" } });
    expect(after.style.clipPath).toMatch(/ 20%\)$/);
  });

  it("lays one over the other with an onion skin", async () => {
    const { user, view } = await openImage("Content/UI/HarborMap.png");
    await within(view).findByRole("img", { name: "Before" });
    await user.click(within(view).getByRole("button", { name: "Onion skin" }));
    const after = within(view).getByRole("img", { name: "After" });
    expect(after.style.opacity).toBe("0.5");
    fireEvent.change(within(view).getByRole("slider", { name: "Opacity of after" }), { target: { value: "80" } });
    expect(after.style.opacity).toBe("0.8");
    expect(within(view).getByRole("img", { name: "Before" })).toBeInTheDocument();
  });

  it("shows a file as a revision changed it", async () => {
    const b = createMockBackend({ stepMs: 0 });
    const side = vi.spyOn(b, "imageSide");
    setBackend(b);
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByText(WS));
    await user.click(await screen.findByRole("button", { name: "History" }));
    await user.click(await screen.findByRole("option", { name: /^r1479 / }));
    const details = screen.getByRole("complementary", { name: "Revision" });
    await user.click(await within(details).findByRole("button", { name: "Diff of Content/UI/HarborMap.png" }));
    const view = await screen.findByRole("region", { name: "Diff" });
    expect(await within(view).findByRole("img", { name: "After" })).toBeInTheDocument();
    expect(side).toHaveBeenCalledWith(WS, "Content/UI/HarborMap.png", 1479, "old");
    expect(side).toHaveBeenCalledWith(WS, "Content/UI/HarborMap.png", 1479, "new");
  });

  it("offers only side by side for a new image", async () => {
    const { view } = await openImage("Content/UI/NewIcon.png", "added");
    expect(await within(view).findByRole("img", { name: "After" })).toBeInTheDocument();
    expect(within(view).queryByRole("img", { name: "Before" })).toBeNull();
    expect(within(view).getByRole("button", { name: "Swipe" })).toBeDisabled();
    expect(within(view).getByText("The file is new.")).toBeInTheDocument();
  });

  it("falls back to sizes when the web view cannot show the image", async () => {
    const { view } = await openImage("Content/UI/HarborMap.webp");
    fireEvent.error(await within(view).findByRole("img", { name: "Before" }));
    const note = await within(view).findByRole("note");
    expect(note).toHaveTextContent("cannot show this image format");
    expect(note).toHaveTextContent("Before");
  });

  it("falls back to sizes for images over 64 MB", async () => {
    const { view } = await openImage("Content/UI/HugePanorama.png");
    const note = await within(view).findByRole("note");
    expect(note).toHaveTextContent("Images over 64 MB are compared by size only.");
    expect(within(view).queryByRole("img")).toBeNull();
  });

  it("keeps other binary files as a note", async () => {
    const { view } = await openImage("Content/Audio/Waves.wav");
    expect(await within(view).findByRole("note")).toHaveTextContent("Binary file");
    await waitFor(() => expect(within(view).queryByRole("img")).toBeNull());
  });
});

describe("image side transfer", () => {
  it("reads the tag, the size and the content", () => {
    const none = new Uint8Array(9).buffer;
    expect(decodeImageSide(none, "a.png")).toBeNull();
    const big = new Uint8Array(9);
    big[0] = 1;
    new DataView(big.buffer).setBigUint64(1, 70_000_000n, true);
    expect(decodeImageSide(big.buffer, "a.png")).toEqual({ size: 70_000_000, url: null });
    const small = new Uint8Array([2, 3, 0, 0, 0, 0, 0, 0, 0, 7, 8, 9]);
    const created = vi.fn(() => "blob:x");
    const orig = URL.createObjectURL;
    URL.createObjectURL = created;
    try {
      expect(decodeImageSide(small.buffer, "a.png")).toEqual({ size: 3, url: "blob:x" });
      const blob = (created.mock.calls[0] as unknown as [Blob])[0];
      expect(blob.type).toBe("image/png");
      expect(blob.size).toBe(3);
    } finally {
      URL.createObjectURL = orig;
    }
  });
});
