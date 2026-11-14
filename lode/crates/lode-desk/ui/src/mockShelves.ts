// Made-up shelves for the mock backend, in the shape of `lode shelves --json` and
// `lode shelf diff --json`.

import type { ChangeView, MissingCheck, ShelfFileDiff, ShelfInfo } from "./api";
import { BOAT_PATCH } from "./mockDiff";

/** A shelf with what the mock needs besides what the server returns. */
export interface MockShelf extends ShelfInfo {
  changes: ChangeView[];
  diff: ShelfFileDiff[];
  /** Required checks that have not passed for the current version. */
  unmet?: MissingCheck[];
}

function textDiff(path: string, body: string): ShelfFileDiff {
  return { path, patch: `--- a/${path}\n+++ b/${path}\n${body}\n` };
}

const BUOYANCY_CPP = `@@ -14,9 +14,9 @@
 #include "Buoyancy.h"

-float UBuoyancy::SampleWave(const FVector& Where) const
+float UBuoyancy::SampleHeight(const FVector& Where) const
 {
     const float Phase = Where.X * WaveScale + GetWorld()->TimeSeconds * WaveSpeed;
     return FMath::Sin(Phase) * WaveHeight;
 }

@@ -31,5 +31,5 @@
 float UBuoyancy::SampleSlope(const FVector& Where) const
 {
-    return SampleWave(Where + FVector(50.f, 0.f, 0.f)) - SampleWave(Where);
+    return SampleHeight(Where + FVector(50.f, 0.f, 0.f)) - SampleHeight(Where);
 }`;

const BUOYANCY_H = `@@ -18,7 +18,8 @@
 public:
     /** Height of the water surface under a point, in centimetres. */
-    float SampleWave(const FVector& Where) const;
+    float SampleHeight(const FVector& Where) const;
+    UE_DEPRECATED(5.4, "Use SampleHeight") float SampleWave(const FVector& Where) const { return SampleHeight(Where); }
     float SampleSlope(const FVector& Where) const;`;

const GAME_INI = `@@ -4,4 +4,5 @@
 [/Script/Skyfall.MarketSettings]
 StallCount=12
-AwningColor=(R=0.8,G=0.2,B=0.1)
+AwningColor=(R=0.15,G=0.45,B=0.7)
+AwningWindResponse=0.6`;

const modify = (path: string, size: number): ChangeView => ({ path, action: "modify", kind: "file", size });

/** The demo depot's shelves, newest first. */
export function demoShelves(): MockShelf[] {
  return [
    {
      id: 44,
      version: 1,
      branch: "main",
      base_rev: 1481,
      owner: { name: "refactor-bot", kind: "agent" },
      agent_owner: "dave",
      description: "Rename Buoyancy::SampleWave to SampleHeight\n\nThe old name stays as a deprecated wrapper for one release.",
      state: "open",
      created_at: "2026-11-14T08:40:00Z",
      updated_at: "2026-11-14T08:40:00Z",
      changes: [modify("Source/Skyfall/Buoyancy.cpp", 2_412), modify("Source/Skyfall/Buoyancy.h", 1_104)],
      diff: [textDiff("Source/Skyfall/Buoyancy.cpp", BUOYANCY_CPP), textDiff("Source/Skyfall/Buoyancy.h", BUOYANCY_H)],
      unmet: [
        { name: "ci/build", reporter: "ci", reason: "absent" },
        { name: "ci/test", reporter: "ci", reason: "not_success", state: "failure" },
      ],
    },
    {
      id: 42,
      version: 3,
      branch: "main",
      base_rev: 1479,
      owner: { name: "carol", kind: "user" },
      description: "Market stalls: new awnings and repainted crates",
      state: "open",
      created_at: "2026-11-13T15:02:00Z",
      updated_at: "2026-11-14T07:55:00Z",
      changes: [
        modify("Config/DefaultGame.ini", 412),
        modify("Content/Maps/Market.umap", 48_220_160),
        modify("Content/Props/Crate_02.uasset", 2_314_240),
      ],
      diff: [
        textDiff("Config/DefaultGame.ini", GAME_INI),
        { path: "Content/Maps/Market.umap", skipped: "too_large" },
        { path: "Content/Props/Crate_02.uasset", skipped: "binary" },
      ],
    },
    {
      id: 38,
      version: 2,
      branch: "main",
      base_rev: 1476,
      owner: { name: "alice", kind: "user" },
      description: "Boat tilt experiment: pitch follows the wave slope",
      state: "open",
      created_at: "2026-11-12T17:20:00Z",
      updated_at: "2026-11-13T10:05:00Z",
      changes: [modify("Source/Skyfall/Boat.cpp", 1_380)],
      diff: [textDiff("Source/Skyfall/Boat.cpp", BOAT_PATCH)],
    },
    {
      id: 30,
      version: 1,
      branch: "main",
      base_rev: 1466,
      owner: { name: "alice", kind: "user" },
      description: "Harbor lights pass",
      state: "submitted",
      submitted_rev: 1470,
      created_at: "2026-11-10T11:00:00Z",
      updated_at: "2026-11-10T16:30:00Z",
      changes: [modify("Content/Maps/Harbor.umap", 51_200_000)],
      diff: [{ path: "Content/Maps/Harbor.umap", skipped: "too_large" }],
    },
    {
      id: 27,
      version: 1,
      branch: "main",
      base_rev: 1460,
      owner: { name: "bob", kind: "user" },
      description: "Try a darker fog",
      state: "deleted",
      created_at: "2026-11-09T09:30:00Z",
      updated_at: "2026-11-09T12:00:00Z",
      changes: [modify("Config/DefaultEngine.ini", 3_200)],
      diff: [textDiff("Config/DefaultEngine.ini", "@@ -40,1 +40,1 @@\n-r.Fog.Density=0.02\n+r.Fog.Density=0.05")],
    },
  ];
}
