// Renders each screen of the app against the mocked backend, in light and dark, into
// ui/screenshots/. Run with `npm run screenshots` (builds the mocked app first). Needs
// Playwright and a Chromium: set CHROMIUM_PATH to use a browser installed elsewhere.

import { createServer } from "node:http";
import { mkdir, readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");

const here = fileURLToPath(new URL(".", import.meta.url));
const dist = join(here, "..", "dist-mock");
const out = join(here, "..", "screenshots");
const SERVER = "https://lode.example.com";

const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml" };

function serve() {
  const server = createServer(async (req, res) => {
    const path = normalize(decodeURIComponent(new URL(req.url, "http://x").pathname)).replace(/^([/\\])+/, "");
    try {
      const body = await readFile(join(dist, path === "" ? "index.html" : path));
      res.writeHead(200, { "content-type": types[extname(path)] ?? "text/html" });
      res.end(body);
    } catch {
      res.writeHead(404).end();
    }
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server)));
}

const scenes = {
  async welcome() {},
  async "sign-in"(page) {
    await page.getByRole("button", { name: "Sign in to a server…" }).click();
    const d = page.getByRole("dialog");
    await d.getByLabel("Server").fill(SERVER);
    await d.getByLabel("Name").fill("alice");
    await d.getByLabel("Password").fill("correct horse");
  },
  async "sign-in-error"(page) {
    await scenes["sign-in"](page);
    const d = page.getByRole("dialog");
    await d.getByLabel("Password").fill("wrong");
    await d.getByRole("button", { name: "Sign in" }).click();
    await d.getByRole("alert").waitFor();
  },
  async "check-out"(page) {
    await page.getByRole("button", { name: "Check out a depot…" }).click();
    const d = page.getByRole("dialog");
    await d.getByLabel(/Depot/).fill(`${SERVER}/skyfall`);
    await d.getByLabel("Folder").fill("D:\\Work\\Skyfall-2");
    await d.getByRole("button", { name: "Check out" }).click();
    // Half way through the mocked download.
    await page.waitForFunction(() => {
      const bar = document.querySelector('[role="progressbar"]');
      return bar && Number(bar.getAttribute("aria-valuenow")) >= Number(bar.getAttribute("aria-valuemax")) / 2;
    });
  },
  async workspace(page) {
    // Two workspaces open, the first one showing.
    await page.getByText("D:\\Work\\Skyfall", { exact: true }).click();
    await page.getByRole("group", { name: "Changes" }).waitFor();
    await page.getByRole("button", { name: "Home" }).click();
    await page.getByText("D:\\Tools\\pipeline", { exact: true }).click();
    await page.getByRole("heading", { name: "pipeline" }).waitFor();
    await page.locator(".tab-label", { hasText: "skyfall" }).click();
    await page.getByRole("group", { name: "Changes" }).waitFor();
  },
  async "changes-picked"(page) {
    await scenes.workspace(page);
    await page.getByRole("checkbox", { name: "Pick all of Changes" }).click();
    await page.getByRole("checkbox", { name: "Pick Content/Props/Crate_02.uasset" }).click();
    await page.getByText("Boat.cpp").click();
    await page.getByLabel("Commit message").fill("Harbor: new mooring for the boat");
  },
  async "commit-refused"(page) {
    await scenes.workspace(page);
    await page.getByRole("checkbox", { name: "Pick all of Changes" }).click();
    await page.getByLabel("Commit message").fill("Crates");
    await page.getByRole("button", { name: "Commit 3 files" }).click();
    await page.getByRole("form", { name: "Commit" }).getByRole("alert").waitFor();
  },
  async history(page) {
    // main's history with the merge from feature/harbor-night selected.
    await scenes.workspace(page);
    await page.getByRole("button", { name: "History", exact: true }).click();
    await page.getByRole("option", { name: /^r1478 / }).click();
    await page.getByRole("list", { name: "Changed files" }).waitFor();
  },
  async checks(page) {
    // A revision whose tests failed: the badge on the row and each check in the details.
    await scenes.workspace(page);
    await page.getByRole("button", { name: "History", exact: true }).click();
    await page.getByRole("option", { name: /^r1481 / }).click();
    await page.getByRole("list", { name: "Checks" }).waitFor();
  },
  async "image-side"(page) {
    // The harbor map as the night lighting pass repainted it.
    await scenes.workspace(page);
    await page.getByRole("button", { name: "History", exact: true }).click();
    await page.getByRole("option", { name: /^r1479 / }).click();
    await page.getByRole("button", { name: "Diff of Content/UI/HarborMap.png" }).click();
    await page.getByRole("img", { name: "After" }).waitFor();
    await page.getByText("640 × 400").first().waitFor();
  },
  async "image-swipe"(page) {
    await scenes["image-side"](page);
    await page.getByRole("button", { name: "Swipe" }).click();
    await page.getByRole("slider", { name: "Swipe position" }).fill("42");
    await page.getByText("640 × 400").first().waitFor();
  },
  async "image-onion"(page) {
    await scenes["image-side"](page);
    await page.getByRole("button", { name: "Onion skin" }).click();
    await page.getByText("640 × 400").first().waitFor();
  },
  async "history-release"(page) {
    // A release branch: its own revisions, then main's below the branch point.
    await page.getByText("D:\\Work\\Skyfall-release", { exact: true }).click();
    await page.getByRole("button", { name: "History", exact: true }).click();
    const list = page.getByRole("listbox", { name: "Revisions" });
    await list.getByRole("option").first().waitFor();
    await list.evaluate((el) => {
      el.scrollTop = 6 * 40;
    });
    await page.getByRole("option", { name: /^r1390 / }).click();
    await page.getByRole("list", { name: "Changed files" }).waitFor();
  },
  async "file-history"(page) {
    await scenes.history(page);
    await page.getByRole("button", { name: /^History of Source\// }).first().click();
    await page.getByRole("heading", { name: /^History of / }).waitFor();
    await page.getByRole("list", { name: "Changed files" }).waitFor();
  },
  async blame(page) {
    await scenes.workspace(page);
    await page.getByRole("region", { name: "Pending changes" }).getByText("Boat.cpp").click();
    await page.getByRole("complementary", { name: "Details" }).getByRole("button", { name: "Blame" }).click();
    await page.getByText(/ABoat::Tick/).waitFor();
  },
  async diff(page) {
    await scenes.workspace(page);
    await page.getByRole("region", { name: "Pending changes" }).getByText("Boat.cpp").click();
    await page.getByRole("button", { name: "Show diff" }).click();
    await page.getByRole("table", { name: "Unified diff" }).waitFor();
  },
  async "diff-split"(page) {
    await scenes.diff(page);
    await page.getByRole("button", { name: "Side by side" }).click();
    await page.getByRole("table", { name: "Side-by-side diff" }).waitFor();
    // The choice is remembered; leave the next scenes on the default.
    await page.evaluate(() => localStorage.clear());
  },
  async "diff-binary"(page) {
    await scenes.workspace(page);
    await page.getByRole("region", { name: "Pending changes" }).getByText("Harbor.umap").dblclick();
    await page.getByRole("note").waitFor();
  },
  async "diff-revision"(page) {
    await scenes.history(page);
    await page.getByRole("button", { name: "Diff of Source/Skyfall/Boat.h" }).click();
    await page.getByRole("table", { name: "Unified diff" }).waitFor();
  },
  async locks(page) {
    await scenes.workspace(page);
    await page.getByRole("button", { name: /^Locks/ }).click();
    await page.getByText("Content/Maps/Docks.umap").waitFor();
  },
  async "lock-refused"(page) {
    await scenes.workspace(page);
    await page.getByRole("region", { name: "Pending changes" }).getByText("Crate_02.uasset").click();
    await page.getByRole("complementary", { name: "Details" }).getByRole("button", { name: "Lock…" }).click();
    const d = page.getByRole("dialog");
    await d.getByLabel(/Comment/).fill("New crate colours");
    await d.getByRole("button", { name: "Lock" }).click();
    await d.getByRole("list", { name: "Held by others" }).waitFor();
  },
  async changelists(page) {
    await scenes.workspace(page);
    const pending = page.getByRole("region", { name: "Pending changes" });
    await pending.getByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" }).click();
    await pending.getByRole("button", { name: "New changelist…" }).click();
    const d = page.getByRole("dialog");
    await d.getByLabel(/^Name/).fill("buoyancy");
    await d.getByLabel("Description").fill("Rough water: boats roll and pitch with the waves");
    await d.getByRole("button", { name: "Create" }).click();
    await pending.getByRole("group", { name: "Changelist buoyancy" }).waitFor();
    await pending.getByText("Harbor.umap").click({ button: "right" });
    await page.getByRole("menu").waitFor();
  },
  async "commit-changelist"(page) {
    await scenes.workspace(page);
    await page.getByRole("button", { name: "Commit changelist sails" }).click();
    await page.getByRole("dialog").waitFor();
  },
  async shelves(page) {
    await scenes.workspace(page);
    await page.getByRole("button", { name: "Shelves" }).click();
    await page.getByRole("article", { name: "Shelf 44" }).waitFor();
  },
  async "shelf-checks"(page) {
    await scenes.shelves(page);
    await page.getByRole("article", { name: "Shelf 44" }).getByRole("button", { name: "Submit…" }).click();
    const d = page.getByRole("dialog");
    await d.getByRole("button", { name: "Submit" }).click();
    await d.getByRole("list", { name: "Missing checks" }).waitFor();
  },
  async shelve(page) {
    await scenes.workspace(page);
    const pending = page.getByRole("region", { name: "Pending changes" });
    await pending.getByRole("checkbox", { name: "Pick Source/Skyfall/Boat.cpp" }).click();
    await pending.getByRole("button", { name: "Shelve…" }).click();
    const d = page.getByRole("dialog");
    await d.getByLabel("Description").fill("Rough water: boats roll and pitch with the waves");
    await d.getByLabel(/Put on/).waitFor();
  },
  async branches(page) {
    await scenes.workspace(page);
    await page.getByRole("button", { name: /: change branch$/ }).click();
    const d = page.getByRole("dialog", { name: "Branches" });
    await d.getByRole("checkbox", { name: "Show archived" }).check();
    await d.getByRole("option", { name: /^proto\/boats/ }).waitFor();
    await d.getByRole("option", { name: /^release-1\.2/ }).click();
  },
  async "new-branch"(page) {
    await scenes.workspace(page);
    await page.getByRole("button", { name: /: change branch$/ }).click();
    await page.getByRole("dialog", { name: "Branches" }).getByRole("button", { name: "New branch…" }).click();
    const d = page.getByRole("dialog", { name: "New branch" });
    await d.getByRole("textbox", { name: /^Name/ }).fill("fix/harbor-lights");
    await d.getByRole("radio", { name: "Another branch or revision" }).check();
    await d.getByRole("combobox", { name: "Branch to start from" }).selectOption("release-1.2");
    await d.getByRole("textbox", { name: "Revision to start from" }).fill("r1400");
  },
  async switched(page) {
    await scenes.workspace(page);
    await page.getByRole("button", { name: /: change branch$/ }).click();
    const d = page.getByRole("dialog", { name: "Branches" });
    await d.getByRole("option", { name: /^release-1\.2/ }).click();
    await d.getByRole("button", { name: "Switch" }).click();
    await page.getByRole("dialog", { name: "Switched to release-1.2" }).waitFor();
  },
  async merged(page) {
    await scenes.workspace(page);
    await page.getByRole("button", { name: /: change branch$/ }).click();
    const d = page.getByRole("dialog", { name: "Branches" });
    await d.getByRole("option", { name: /^release-1\.2/ }).click();
    await d.getByRole("button", { name: "Merge…" }).click();
    await page.getByRole("dialog", { name: "Merge into main" }).getByRole("button", { name: "Merge" }).click();
    await page.getByRole("dialog", { name: /^Merged / }).waitFor();
  },
  async "three-way"(page) {
    await scenes.merged(page);
    await page.getByRole("button", { name: "Resolve conflicts" }).click();
    await page.getByRole("region", { name: "Pending changes" }).getByText("Boat.cpp").dblclick();
    const view = page.getByRole("region", { name: "Resolve conflict" });
    await view.getByRole("group", { name: "Conflict 1 of 2" }).getByRole("button", { name: "Use mine" }).click();
    await view.getByRole("group", { name: "Conflict 1 of 1" }).waitFor();
  },
  async "open-error"(page) {
    // The mocked folder picker returns a folder that is not a workspace.
    await page.getByRole("button", { name: "Open a workspace folder…" }).click();
    await page.getByRole("alert").waitFor();
  },
};

const server = await serve();
const url = `http://127.0.0.1:${server.address().port}/`;
await mkdir(out, { recursive: true });
const browser = await chromium.launch(process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {});
try {
  for (const scheme of ["light", "dark"]) {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 2, colorScheme: scheme });
    // SCENES=a,b renders only those scenes.
    const only = process.env.SCENES?.split(",").filter(Boolean);
    for (const [name, scene] of Object.entries(scenes).filter(([n]) => !only || only.includes(n))) {
      const page = await context.newPage();
      await page.goto(url);
      await page.getByRole("button", { name: "Check out a depot…" }).waitFor();
      await scene(page);
      await page.screenshot({ path: join(out, `${name}-${scheme}.png`) });
      await page.close();
      console.log(`${name}-${scheme}.png`);
    }
    await context.close();
  }
} finally {
  await browser.close();
  server.close();
}
