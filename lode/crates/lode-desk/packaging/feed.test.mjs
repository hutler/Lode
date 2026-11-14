// node --test packaging/feed.test.mjs
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { buildFeed, readBundles } from "./feed.mjs";

const SIG = Buffer.from("untrusted comment: signature from tauri secret key\nRUQ...\n").toString("base64");
const BASE = "https://github.com/hutler/simpledevops/releases/download/lode-v0.2.0";

test("signed bundles become platforms, unsigned ones are left out", () => {
  const dir = mkdtempSync(join(tmpdir(), "feed-"));
  for (const name of [
    "lode-desk-0.2.0-linux-x86_64.AppImage",
    "lode-desk-0.2.0-linux-x86_64.deb",
    "lode-desk-0.2.0-windows-x86_64-setup.exe",
    "lode-desk-0.2.0-macos-universal.dmg",
    "lode-desk-0.2.0-macos-universal.app.tar.gz",
  ]) {
    writeFileSync(join(dir, name), "x");
  }
  for (const name of [
    "lode-desk-0.2.0-linux-x86_64.AppImage",
    "lode-desk-0.2.0-windows-x86_64-setup.exe",
    "lode-desk-0.2.0-macos-universal.app.tar.gz",
  ]) {
    writeFileSync(join(dir, `${name}.sig`), `${SIG}\n`);
  }
  const feed = buildFeed({
    version: "0.2.0",
    baseUrl: `${BASE}/`,
    notes: "New",
    date: "2026-11-20T10:00:00Z",
    files: readBundles(dir),
  });
  assert.deepEqual(Object.keys(feed.platforms).sort(), [
    "darwin-aarch64",
    "darwin-aarch64-app",
    "darwin-x86_64",
    "darwin-x86_64-app",
    "linux-x86_64",
    "linux-x86_64-appimage",
    "windows-x86_64-nsis",
  ]);
  assert.equal(feed.platforms["linux-x86_64"].url, `${BASE}/lode-desk-0.2.0-linux-x86_64.AppImage`);
  assert.equal(feed.platforms["windows-x86_64-nsis"].signature, SIG);
});

test("refuses a feed with nothing signed, a bad version or a plain http address", () => {
  const files = { "lode-desk-0.2.0-linux-x86_64.deb": SIG };
  assert.throws(() => buildFeed({ version: "0.2.0", baseUrl: BASE, notes: "", date: "", files: {} }), /no signed/);
  assert.throws(() => buildFeed({ version: "v0.2", baseUrl: BASE, notes: "", date: "", files }), /release version/);
  assert.throws(() => buildFeed({ version: "0.2.0", baseUrl: "http://x", notes: "", date: "", files }), /https/);
  // Files of another version are not this release's.
  assert.throws(() => buildFeed({ version: "0.3.0", baseUrl: BASE, notes: "", date: "", files }), /no signed/);
});
