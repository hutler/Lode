import { describe, expect, it } from "vitest";

import cases from "../linkCases.json";
import { build, encode, type Link, MAX_LINK, parse, sameServer, tryParse } from "../links";

type Case = { text: string; build?: boolean; link: Link | null };
const all = cases.cases as Case[];

describe("lode links", () => {
  it("read every shared case as listed", () => {
    expect(all.length).toBeGreaterThan(40);
    for (const c of all) {
      if (c.link === null) expect(() => parse(c.text), c.text).toThrow();
      else expect(parse(c.text), c.text).toEqual(c.link);
    }
  });

  it("build links that read back to what they were built from", () => {
    for (const c of all) {
      if (c.link === null) continue;
      const text = build(c.link);
      if (c.build) expect(text).toBe(c.text);
      expect(parse(text)).toEqual(c.link);
    }
  });

  it("refuse links that are too long or cannot be written", () => {
    expect(() => parse(`lode://h/game/${"a".repeat(MAX_LINK)}@1`)).toThrow("too long");
    const file = (server: string, path: string, rev: number | null, branch: string | null): Link => ({
      kind: "file",
      server,
      depot: "game",
      path,
      rev,
      branch,
    });
    expect(() => build(file("https://h", "a.txt", null, null))).toThrow();
    expect(() => build(file("https://h/lode", "a.txt", 1, null))).toThrow();
    expect(() => build(file("ftp://h", "a.txt", 1, null))).toThrow();
    expect(() => build(file("https://h", "a/../b", 1, null))).toThrow();
    expect(() => build(file("https://h", "r1", null, "main"))).toThrow();
    expect(build(file("https://h", "r1", 3, "main"))).toBe("lode://h/game/r1@3?branch=main");
    expect(build(file("https://H:443/", "a b/c.txt", 2, null))).toBe("lode://h:443/game/a%20b/c.txt@2");
    expect(tryParse("lode://h/game")).toEqual({ error: "the link names nothing to open" });
  });

  it("encode everything but unreserved characters", () => {
    expect(encode("a!'()*b ä/")).toBe("a%21%27%28%29%2Ab%20%C3%A4%2F");
    expect(sameServer("https://Lode.example.com:443/", "https://lode.example.com")).toBe(true);
    expect(sameServer("http://lode.example.com", "https://lode.example.com")).toBe(false);
  });
});
