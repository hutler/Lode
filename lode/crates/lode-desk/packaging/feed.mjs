#!/usr/bin/env node
// Writes the Lode Desk update feed (`latest.json`) from a folder of release files and their
// `.sig` signatures, as the Tauri updater reads it:
//
//   node feed.mjs DIR VERSION BASE_URL NOTES_FILE > latest.json
//
// DIR holds files named `lode-desk-VERSION-PLATFORM.EXT` (see BUNDLES) with `FILE.sig` next to
// each one that can update an installed app. BASE_URL is where those files are downloaded from,
// the release's download folder. A bundle without a signature is left out of the feed.

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

/** File name endings, and the updater platforms each one serves. The first platform of a kind
 * is also the fallback for installs whose kind the updater does not know. */
export const BUNDLES = [
  { suffix: "linux-x86_64.AppImage", platforms: ["linux-x86_64", "linux-x86_64-appimage"] },
  { suffix: "linux-x86_64.deb", platforms: ["linux-x86_64-deb"] },
  { suffix: "linux-x86_64.rpm", platforms: ["linux-x86_64-rpm"] },
  { suffix: "windows-x86_64.msi", platforms: ["windows-x86_64", "windows-x86_64-msi"] },
  { suffix: "windows-x86_64-setup.exe", platforms: ["windows-x86_64-nsis"] },
  {
    suffix: "macos-universal.app.tar.gz",
    platforms: ["darwin-x86_64", "darwin-aarch64", "darwin-x86_64-app", "darwin-aarch64-app"],
  },
];

/** The feed for `files` (name to signature text, or null when unsigned). */
export function buildFeed({ version, baseUrl, notes, date, files }) {
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`\`${version}\` is not a release version`);
  const base = baseUrl.replace(/\/+$/, "");
  if (!base.startsWith("https://")) throw new Error("the download address must be https");
  const platforms = {};
  for (const [name, signature] of Object.entries(files)) {
    if (signature === null) continue;
    const bundle = BUNDLES.find((b) => name === `lode-desk-${version}-${b.suffix}`);
    if (!bundle) continue;
    for (const p of bundle.platforms) {
      platforms[p] = { signature: signature.trim(), url: `${base}/${encodeURIComponent(name)}` };
    }
  }
  if (Object.keys(platforms).length === 0) throw new Error("no signed bundle to put in the feed");
  return { version, notes, pub_date: date, platforms };
}

/** Reads DIR into the `files` map of {@link buildFeed}. */
export function readBundles(dir) {
  const names = readdirSync(dir);
  const files = {};
  for (const name of names) {
    if (name.endsWith(".sig")) continue;
    files[name] = names.includes(`${name}.sig`) ? readFileSync(join(dir, `${name}.sig`), "utf8") : null;
  }
  return files;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  const [dir, version, baseUrl, notesFile] = process.argv.slice(2);
  if (!dir || !version || !baseUrl) {
    console.error("usage: node feed.mjs DIR VERSION BASE_URL [NOTES_FILE]");
    process.exit(2);
  }
  const notes = notesFile ? readFileSync(notesFile, "utf8").trim() : "";
  const date = new Date().toISOString().replace(/\.\d{3}Z$/, "Z");
  const feed = buildFeed({ version, baseUrl, notes, date, files: readBundles(dir) });
  process.stdout.write(`${JSON.stringify(feed, null, 2)}\n`);
}
