// Made-up check results for the mock backend, in the shape of `lode checks --json`.

import type { CheckInfo } from "./api";

const CI = { name: "ci", kind: "user" as const };
const AT = "2026-11-14T08:50:00Z";

function check(target: string, name: string, state: CheckInfo["state"], summary?: string): CheckInfo {
  const slug = target.replace("@", "-v");
  return { name, state, summary, url: `https://ci.example.com/skyfall/${slug}/${name.replace("/", "-")}`, reporter: CI, updated_at: AT };
}

/** The checks of a demo target: `rN` or `ID@VERSION`. */
export function demoChecks(target: string): CheckInfo[] {
  const shelf: Record<string, CheckInfo[]> = {
    "44@1": [check(target, "ci/test", "failure", "2 tests failed: BuoyancyTest.SampleHeight, BoatTest.Roll")],
    "42@3": [check(target, "ci/build", "success", "Built in 6 min"), check(target, "ci/test", "pending", "Running, 40% done")],
    "38@2": [check(target, "ci/build", "success", "Built in 5 min"), check(target, "ci/test", "success", "412 tests passed")],
  };
  if (!target.startsWith("r")) return shelf[target] ?? [];
  const n = Number(target.slice(1));
  if (!Number.isFinite(n) || n % 7 === 0) return [];
  if (n === 1482) return [check(target, "ci/build", "success", "Built in 6 min"), check(target, "ci/test", "pending", "Running")];
  if (n === 1481) {
    return [check(target, "ci/build", "success", "Built in 6 min"), check(target, "ci/test", "failure", "3 tests failed")];
  }
  if (n % 11 === 0) return [check(target, "ci/build", "error", "The build agent lost its connection")];
  const out = [check(target, "ci/build", "success", "Built in 5 min"), check(target, "ci/test", "success", "412 tests passed")];
  if (n % 3 === 0) {
    out.push({ ...check(target, "review/agent", "success", "No problems found"), reporter: { name: "review-bot", kind: "agent", owner: "dave" } });
  }
  return out;
}
