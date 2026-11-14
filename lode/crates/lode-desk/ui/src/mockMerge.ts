// Made-up conflicts for the mock backend: the boat code changed on both branches (two regions
// that do not merge, one change that does), and binary maps changed on both.

import type { ConflictInfo } from "./api";

const HEAD = `#include "Boat.h"
#include "Water/Buoyancy.h"
`;

const CTOR = `
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
`;

const BASE_ROLL = `    Roll = FMath::FInterpTo(Roll, Wave * 0.2f, DeltaTime, 2.f);
`;
const MINE_ROLL = `    Roll = FMath::FInterpTo(Roll, Wave * 0.35f, DeltaTime, 3.f);
    Pitch = FMath::FInterpTo(Pitch, Wave * 0.1f, DeltaTime, 3.f);
`;
const THEIRS_ROLL = `    Roll = FMath::FInterpTo(Roll, Wave * RollScale, DeltaTime, 2.f);
`;

const SAIL = `}

void ABoat::SetSail(float Amount)
{
`;
const BASE_SAIL = `    Sail = FMath::Clamp(Amount, 0.f, 1.f);
`;
const MINE_SAIL = `    Sail = FMath::Clamp(Amount, 0.f, MaxSail);
`;
const THEIRS_SAIL = `    Sail = FMath::Clamp(Amount, 0.f, 1.f);
    OnSailChanged.Broadcast(Sail);
`;
const NET = `#include "Net/UnrealNetwork.h"
`;

function side(text: string) {
  return { size: new TextEncoder().encode(text).length, text };
}

/** A text conflict in the boat code, theirs from `rev`. */
function boat(path: string, rev: number): ConflictInfo {
  const label = `theirs r${rev}`;
  const base = HEAD + CTOR + BASE_ROLL + SAIL + BASE_SAIL + "}\n";
  const mine = HEAD + CTOR + MINE_ROLL + SAIL + MINE_SAIL + "}\n";
  const theirs = HEAD + NET + CTOR + THEIRS_ROLL + SAIL + THEIRS_SAIL + "}\n";
  const result =
    HEAD +
    NET +
    CTOR +
    `<<<<<<< mine\n${MINE_ROLL}=======\n${THEIRS_ROLL}>>>>>>> ${label}\n` +
    SAIL +
    `<<<<<<< mine\n${MINE_SAIL}=======\n${THEIRS_SAIL}>>>>>>> ${label}\n` +
    "}\n";
  return {
    path,
    theirs_rev: rev,
    text: true,
    base: side(base),
    mine: side(mine),
    theirs: side(theirs),
    result: side(result),
    theirs_file: null,
  };
}

/** A binary conflict: the local file stays, the incoming one is beside it. */
function binary(path: string, rev: number): ConflictInfo {
  const mine = { size: 48_210_334, text: null };
  return {
    path,
    theirs_rev: rev,
    text: false,
    base: null,
    mine,
    theirs: { size: 48_502_117, text: null },
    result: mine,
    theirs_file: `${path}.theirs-r${rev}`,
  };
}

/** The demo conflict of `path`, with theirs from `rev`. */
export function demoConflict(path: string, rev: number): ConflictInfo {
  return /\.(cpp|h|cs|ini|txt|md)$/i.test(path) ? boat(path, rev) : binary(path, rev);
}
