// Made-up diffs for the mock backend: text patches for sources and config, sizes only for
// assets, in the shape of `lode diff --json`.

import type { FileDiff } from "./api";

const BINARY = /\.(uasset|umap|png|jpe?g|gif|webp|bmp|wav|fbx)$/;

function binary(path: string, old: number | null, now: number | null): FileDiff {
  return {
    path,
    status: old === null ? "added" : now === null ? "deleted" : "modified",
    old_path: old === null ? null : path,
    binary: true,
    old_size: old,
    new_size: now,
    old_mode: old === null ? null : "100644",
    new_mode: now === null ? null : "100644",
    insertions: 0,
    deletions: 0,
    patch: `diff --lode a/${path} b/${path}\nBinary files a/${path} and b/${path} differ (${old ?? 0} -> ${now ?? 0} bytes)\n`,
  };
}

function text(path: string, status: FileDiff["status"], body: string): FileDiff {
  const lines = body.split("\n");
  const ins = lines.filter((l) => l.startsWith("+") && !l.startsWith("+++")).length;
  const del = lines.filter((l) => l.startsWith("-") && !l.startsWith("---")).length;
  const from = status === "added" ? "/dev/null" : `a/${path}`;
  const to = status === "deleted" ? "/dev/null" : `b/${path}`;
  return {
    path,
    status,
    old_path: status === "added" ? null : path,
    binary: false,
    old_size: status === "added" ? null : 1200,
    new_size: status === "deleted" ? null : 1200 + ins * 30 - del * 30,
    old_mode: status === "added" ? null : "100644",
    new_mode: status === "deleted" ? null : "100644",
    insertions: ins,
    deletions: del,
    patch: `diff --lode a/${path} b/${path}\n--- ${from}\n+++ ${to}\n${body}\n`,
  };
}

export const BOAT_PATCH = `@@ -9,13 +9,17 @@

 void ABoat::Tick(float DeltaTime)
 {
     Super::Tick(DeltaTime);
-    const float Wave = Buoyancy->SampleWave(GetActorLocation());
-    AddActorWorldOffset(FVector(0.f, 0.f, Wave * DeltaTime));
-    Roll = FMath::FInterpTo(Roll, Wave * 0.2f, DeltaTime, 2.f);
+    const FVector Here = GetActorLocation();
+    const float Wave = Buoyancy->SampleWave(Here) * RoughWaterScale;
+    AddActorWorldOffset(FVector(0.f, 0.f, Wave * DeltaTime));
+    Roll = FMath::FInterpTo(Roll, Wave * 0.35f, DeltaTime, 1.5f);
+    Pitch = FMath::FInterpTo(Pitch, Buoyancy->SampleSlope(Here) * 0.1f, DeltaTime, 1.5f);
 }

 void ABoat::SetSail(float Amount)
 {
-    Sail = FMath::Clamp(Amount, 0.f, 1.f);
+    // Half sail is the most a boat can carry in a storm.
+    Sail = FMath::Clamp(Amount, 0.f, bStorm ? 0.5f : 1.f);
 }`;

const SAIL_ADDED = `@@ -0,0 +1,9 @@
+#include "Sail.h"
+
+void USail::Flap(float Wind)
+{
+    // Flap harder when the wind turns across the sail.
+    const float Across = FMath::Abs(FVector::DotProduct(WindDirection, GetRightVector()));
+    FlapAmount = FMath::FInterpTo(FlapAmount, Across * Wind, GetWorld()->DeltaTimeSeconds, 4.f);
+}
+`;

/** The working-copy diff of a demo file, or null when it has no change. */
export function demoWorkingDiff(path: string, status: string): FileDiff | null {
  if (status === "untracked" || status === "unchanged") return null;
  if (BINARY.test(path)) return binary(path, status === "added" ? null : 12_480_512, status === "deleted" ? null : 12_517_376);
  if (path.endsWith("Boat.cpp")) return text(path, "modified", BOAT_PATCH);
  if (path.endsWith("Sail.cpp")) return text(path, "added", SAIL_ADDED);
  return text(path, "modified", `@@ -1,3 +1,3 @@\n [Settings]\n-Value=1\n+Value=2\n `);
}

/** The diff of `path` as revision `rev` changed it. */
export function demoRevisionDiff(rev: number, path: string, action: "add" | "modify" | "delete"): FileDiff {
  if (BINARY.test(path)) {
    const before = 2_000_000 + (rev % 97) * 10_000;
    return binary(path, action === "add" ? null : before, action === "delete" ? null : before + 4_096);
  }
  if (action === "add") return text(path, "added", `@@ -0,0 +1,3 @@\n+// ${path}\n+\n+// Added in r${rev}.`);
  if (action === "delete") return text(path, "deleted", `@@ -1,3 +0,0 @@\n-// ${path}\n-\n-// Removed in r${rev}.`);
  if (path.endsWith(".ini")) {
    return text(
      path,
      "modified",
      `@@ -12,6 +12,6 @@\n [/Script/Engine.RendererSettings]\n r.DefaultFeature.AntiAliasing=2\n-r.Streaming.PoolSize=${1000 + (rev % 7) * 100}\n+r.Streaming.PoolSize=${1100 + (rev % 7) * 100}\n r.Shadow.Virtual.Enable=1\n r.Lumen.Reflections.Allow=1\n `,
    );
  }
  const k = (rev % 5) + 1;
  return text(
    path,
    "modified",
    `@@ -${20 + k},7 +${20 + k},8 @@\n void ABoat::Tick(float DeltaTime)\n {\n     Super::Tick(DeltaTime);\n-    const float Wave = Buoyancy->SampleWave(GetActorLocation());\n+    const float Wave = Buoyancy->SampleWave(GetActorLocation()) * ${k}.0f;\n+    LastWave = Wave;\n     AddActorWorldOffset(FVector(0.f, 0.f, Wave * DeltaTime));\n }\n `,
  );
}
