// Made-up history for the mock backend: a long `main` with merges from feature branches, an
// agent's commits and an imported start, and a release branch that inherits main's history.

import type { BlameResult, ChangeView, PrincipalRef, Revision } from "./api";

const PEOPLE: PrincipalRef[] = [
  { name: "alice", kind: "user" },
  { name: "bob", kind: "user" },
  { name: "carol", kind: "user" },
  { name: "dmitri", kind: "user" },
];
const BOT: PrincipalRef = { name: "refactor-bot", kind: "agent" };

const FILES = [
  "Source/Skyfall/Boat.cpp",
  "Source/Skyfall/Boat.h",
  "Source/Skyfall/Sail.cpp",
  "Content/Maps/Harbor.umap",
  "Content/Props/Crate_02.uasset",
  "Content/Props/Barrel.uasset",
  "Config/DefaultEngine.ini",
  "Source/Skyfall/Weather.cpp",
];

const MESSAGES = [
  "Tune boat buoyancy in rough water",
  "Harbor: move the crane closer to the pier",
  "Fix sail flapping when the wind changes direction",
  "Add barrel variants for the market",
  "Weather: smoother transition from rain to fog",
  "Bump texture budget for the harbor props",
  "Clean up unused includes",
  "Lighting pass on the night harbor",
];

/** The merges on main: `rev` merged `branch` `from_rev..to_rev`. */
export const DEMO_MERGES = [
  { rev: 1478, branch: "feature/harbor-night", from_rev: 1461, to_rev: 1474 },
  { rev: 1451, branch: "feature/weather", from_rev: 1430, to_rev: 1447 },
];

/** The head of each demo branch and where it starts. */
const BRANCHES: Record<string, { head: number; from?: { branch: string; rev: number } }> = {
  main: { head: 1482 },
  "release-1.2": { head: 1404, from: { branch: "main", rev: 1390 } },
};

/** The newest revision of a demo branch. */
export function demoHead(branch: string): number | null {
  return BRANCHES[branch]?.head ?? null;
}

const BASE_TIME = Date.UTC(2026, 10, 14, 9, 30);

function revisionOf(branch: string, rev: number): Revision {
  const merge = branch === "main" ? DEMO_MERGES.find((m) => m.rev === rev) : undefined;
  const owner = BRANCHES[branch]?.from && rev <= (BRANCHES[branch]?.from?.rev ?? 0) ? "main" : branch;
  const agent = rev % 11 === 0;
  const imported = rev <= 40;
  const author = agent ? BOT : PEOPLE[rev % PEOPLE.length];
  return {
    rev,
    branch: owner,
    parent: rev > 1 ? rev - 1 : null,
    author,
    submitter: author,
    created_at: new Date(BASE_TIME - (1482 - rev) * 47 * 60_000).toISOString(),
    message: merge
      ? `Merge ${merge.branch} r${merge.from_rev}..r${merge.to_rev}`
      : imported
        ? `${MESSAGES[rev % MESSAGES.length]} (from the old repository)`
        : agent
          ? `Rename locals to match the style guide in ${FILES[rev % FILES.length].split("/").pop()}`
          : `${MESSAGES[rev % MESSAGES.length]}\n\nTested in the harbor map at dusk and at night.`,
    provenance: imported
      ? { client: "lode-cli/0.1.0", imported: { system: "git", id: (rev * 2654435761).toString(16).padStart(40, "0").slice(0, 40), author: `${author.name} <${author.name}@example.com>`, date: "2025-03-02T10:00:00Z" } }
      : agent
        ? { client: "lode-cli/0.1.0", agent: { name: "refactor-bot", session: `s-${rev}` } }
        : { client: rev % 3 === 0 ? "lode-desk/0.1.0" : "lode-cli/0.1.0" },
    merged_from: merge ? [{ branch: merge.branch, from_rev: merge.from_rev, to_rev: merge.to_rev }] : [],
  };
}

function changesOf(rev: number): ChangeView[] {
  const n = 1 + (rev % 4);
  const out: ChangeView[] = [];
  for (let i = 0; i < n; i++) {
    const path = FILES[(rev + i * 3) % FILES.length];
    if (out.some((c) => c.path === path)) continue;
    out.push({ path, action: i === 2 && rev % 5 === 0 ? "add" : "modify", size: 1000 + rev * 7 });
  }
  return out;
}

/** Whether `rev` changed `path` in the demo history. */
function touches(rev: number, path: string): boolean {
  return changesOf(rev).some((c) => c.path === path);
}

export function demoLog(branch: string, path: string | null, before: number | null, max: number) {
  const head = BRANCHES[branch]?.head ?? 0;
  const out: Revision[] = [];
  let r = Math.min(head, (before ?? head + 1) - 1);
  while (r >= 1 && out.length < max) {
    if (!path || touches(r, path)) out.push(revisionOf(branch, r));
    r -= 1;
  }
  while (r >= 1 && path && !touches(r, path)) r -= 1;
  return { revisions: out, more: r >= 1 };
}

export function demoRevision(branch: string, rev: number): Revision | null {
  const head = BRANCHES[branch]?.head ?? 0;
  if (rev < 1 || rev > head) return null;
  return { ...revisionOf(branch, rev), changes: changesOf(rev) };
}

const BOAT = `#include "Boat.h"
#include "Water/Buoyancy.h"

ABoat::ABoat()
{
    PrimaryActorTick.bCanEverTick = true;
    Buoyancy = CreateDefaultSubobject<UBuoyancy>(TEXT("Buoyancy"));
}

void ABoat::Tick(float DeltaTime)
{
    Super::Tick(DeltaTime);
    const float Wave = Buoyancy->SampleWave(GetActorLocation());
    AddActorWorldOffset(FVector(0.f, 0.f, Wave * DeltaTime));
    Roll = FMath::FInterpTo(Roll, Wave * 0.2f, DeltaTime, 2.f);
}

void ABoat::SetSail(float Amount)
{
    Sail = FMath::Clamp(Amount, 0.f, 1.f);
}`;

export function demoBlame(branch: string, path: string, rev: number | null): BlameResult {
  const at = rev ?? BRANCHES[branch]?.head ?? 1;
  const text = path.endsWith(".cpp") || path.endsWith(".h") ? BOAT : `; ${path}\n[Settings]\nValue=1`;
  const revs = [1120, 1120, 1120, 1188, 1188, 1188, 1301, 1301, 1120, 1120, 1188, 1301, 1477, 1477, 1466, 1301, 1120, 1188, 1188, 1395, 1188].map((r) => Math.min(r, at));
  const lines = text.split("\n").map((t, i) => ({ rev: revs[i % revs.length], text: t }));
  const used = [...new Set(lines.map((l) => l.rev))].sort((a, b) => b - a);
  return {
    path,
    rev: at,
    lines,
    revisions: used.map((r) => {
      const v = revisionOf(branch, r);
      return { rev: r, branch: v.branch, author: v.author, created_at: v.created_at, summary: v.message.split("\n")[0] };
    }),
  };
}
