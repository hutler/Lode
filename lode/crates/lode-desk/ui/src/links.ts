/**
 * `lode://` links: a revision, a file at a revision or on a branch, or a shelf.
 *
 *     lode://HOST[:PORT]/DEPOT/rREV
 *     lode://HOST[:PORT]/DEPOT/PATH@REV        (or PATH?branch=NAME, or both)
 *     lode://HOST[:PORT]/DEPOT/shelf/ID
 *
 * The server is `https://HOST[:PORT]`, or `http://` with `?http=1`. The same rules as
 * `src/links.rs`; both are tested against `linkCases.json`. Anything that is not exactly one of
 * these forms is refused, and a refused link is never acted on.
 */

/** Longest link accepted. */
export const MAX_LINK = 2048;

export type Link =
  | { kind: "revision"; server: string; depot: string; rev: number }
  | { kind: "file"; server: string; depot: string; path: string; rev: number | null; branch: string | null }
  | { kind: "shelf"; server: string; depot: string; id: number };

/** Why a link was refused. */
export class LinkError extends Error {
  override name = "LinkError";
}

function bad(why: string): never {
  throw new LinkError(why);
}

const PLAIN = /^[A-Za-z0-9\-._~!$&'()*+,;=:@%]*$/;
const UNRESERVED = /[A-Za-z0-9\-._~]/;

/** Percent-encodes everything but the unreserved characters, as UTF-8. */
export function encode(text: string): string {
  let out = "";
  for (const b of new TextEncoder().encode(text)) {
    const c = String.fromCharCode(b);
    out += b < 0x80 && UNRESERVED.test(c) ? c : `%${b.toString(16).toUpperCase().padStart(2, "0")}`;
  }
  return out;
}

function decode(text: string): string {
  const bytes: number[] = [];
  for (let i = 0; i < text.length; ) {
    if (text[i] === "%") {
      const hex = text.slice(i + 1, i + 3);
      if (!/^[0-9A-Fa-f]{2}$/.test(hex)) bad(`bad percent escape in \`${text}\``);
      bytes.push(parseInt(hex, 16));
      i += 3;
    } else {
      bytes.push(text.charCodeAt(i));
      i += 1;
    }
  }
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array(bytes));
  } catch {
    return bad(`\`${text}\` is not UTF-8 text`);
  }
}

/** `r123` (or `123` when `r` is optional) with no leading zero, as a number of at least 1. */
function number(text: string, needR: boolean): number | null {
  let digits = text;
  if (text.startsWith("r")) digits = text.slice(1);
  else if (needR) return null;
  if (!/^[1-9][0-9]{0,18}$/.test(digits)) return null;
  const n = Number(digits);
  return Number.isSafeInteger(n) ? n : null;
}

function checkDepot(name: string) {
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(name)) bad(`\`${name}\` is not a depot name`);
}

function checkBranch(name: string) {
  const ok =
    name.length > 0 &&
    name.length <= 200 &&
    /^[A-Za-z0-9._/-]+$/.test(name) &&
    !name.split("/").some((s) => s === "" || s === "." || s === "..");
  if (!ok) bad(`\`${name}\` is not a branch name`);
}

/** One decoded path segment: not empty, `.` or `..`, and free of characters depot paths refuse. */
function segment(raw: string): string {
  const s = decode(raw);
  if (s === "" || s === "." || s === "..") bad("a path has an empty, `.` or `..` part");
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f-\u009f/\\<>:"|?*]/.test(s)) bad(`\`${s}\` has a character depot paths cannot hold`);
  return s;
}

/** `host[:port]`, lowercased; a name, an IPv4 address or a bracketed IPv6 address. */
function authority(text: string): string {
  if (text.includes("@")) bad("links do not carry user names");
  let host: string;
  let port: string | null;
  if (text.startsWith("[")) {
    const end = text.indexOf("]");
    if (end < 0) bad("unclosed `[`");
    const inner = text.slice(1, end);
    if (!/^[0-9A-Fa-f:.]+$/.test(inner)) bad(`\`${inner}\` is not an IPv6 address`);
    const after = text.slice(end + 1);
    if (after === "") port = null;
    else if (after.startsWith(":")) port = after.slice(1);
    else return bad("bad host");
    host = `[${inner.toLowerCase()}]`;
  } else {
    const colon = text.indexOf(":");
    const h = colon < 0 ? text : text.slice(0, colon);
    port = colon < 0 ? null : text.slice(colon + 1);
    const ok = h !== "" && h.split(".").every((label) => /^[A-Za-z0-9-]{1,63}$/.test(label));
    if (!ok) bad(`\`${h}\` is not a host name`);
    host = h.toLowerCase();
  }
  if (port === null) return host;
  const n = /^[0-9]{1,5}$/.test(port) ? Number(port) : 0;
  if (n < 1 || n > 65535) bad(`\`${port}\` is not a port`);
  return `${host}:${n}`;
}

/** Reads a `lode://` link, throwing a {@link LinkError} for anything that is not exactly one of the forms. */
export function parse(text: string): Link {
  if (text.length > MAX_LINK) bad("the link is too long");
  // eslint-disable-next-line no-control-regex
  if (/[^\x21-\x7e]/.test(text)) bad("a link has no spaces or characters that are not percent-encoded");
  if (text.slice(0, 7).toLowerCase() !== "lode://") bad("not a lode:// link");
  let rest = text.slice(7);
  if (rest.includes("#")) bad("links have no `#` part");
  let query: string | null = null;
  const q = rest.indexOf("?");
  if (q >= 0) {
    query = rest.slice(q + 1);
    rest = rest.slice(0, q);
  }
  const slash = rest.indexOf("/");
  if (slash < 0) bad("the link names no depot");
  const host = authority(rest.slice(0, slash));
  const path = rest.slice(slash + 1);

  // The query: `http=1` and, for a file, `branch=NAME`, each at most once.
  let http = false;
  let branch: string | null = null;
  if (query !== null) {
    if (query === "") bad("empty query");
    for (const pair of query.split("&")) {
      const eq = pair.indexOf("=");
      if (eq < 0) bad("bad query");
      const k = pair.slice(0, eq);
      const v = pair.slice(eq + 1);
      if (!/^[A-Za-z0-9\-._~!$&'()*+,;=:@%/?]*$/.test(v)) bad("bad query");
      if (k === "http" && !http && v === "1") http = true;
      else if (k === "branch" && branch === null) {
        const name = decode(v);
        checkBranch(name);
        branch = name;
      } else bad(`\`${pair}\` is not part of a lode link`);
    }
  }
  const server = `${http ? "http" : "https"}://${host}`;

  const raw = path.split("/");
  if (raw.some((s) => !PLAIN.test(s))) bad("bad character in the path");
  const depot = decode(raw[0] ?? "");
  checkDepot(depot);
  const parts = raw.slice(1);
  if (parts.length === 0 || parts.some((p) => p === "")) bad("the link names nothing to open");
  const last = parts[parts.length - 1] ?? "";
  const at = last.split("@").length - 1;
  if (parts.slice(0, -1).some((p) => p.includes("@")) || at > 1) bad("`@` must come once, before the revision");
  if (at === 0 && branch === null) {
    if (parts.length === 1) {
      const rev = number(parts[0] ?? "", true);
      if (rev !== null) return { kind: "revision", server, depot, rev };
    }
    if (parts.length === 2 && parts[0] === "shelf") {
      const raw = parts[1] ?? "";
      const id = raw.startsWith("r") ? null : number(raw, false);
      if (id === null) bad(`\`${raw}\` is not a shelf id`);
      return { kind: "shelf", server, depot, id };
    }
    bad("a file link needs @REV or ?branch=NAME");
  }
  // Without `@REV` these would read as a revision or a shelf link too.
  if (at === 0 && ((parts.length === 1 && number(parts[0] ?? "", true) !== null) || parts[0] === "shelf")) {
    bad("a file link to this path needs @REV");
  }
  let name = last;
  let rev: number | null = null;
  if (at === 1) {
    const i = last.indexOf("@");
    name = last.slice(0, i);
    const r = last.slice(i + 1);
    rev = number(r, false);
    if (rev === null) bad(`\`${r}\` is not a revision`);
  }
  if (name === "") bad("the link names no file");
  const segs = [...parts.slice(0, -1).map(segment), segment(name)];
  return { kind: "file", server, depot, path: segs.join("/"), rev, branch };
}

/** The link, or the reason it is refused. */
export function tryParse(text: string): { link: Link } | { error: string } {
  try {
    return { link: parse(text) };
  } catch (e) {
    return { error: e instanceof Error ? e.message : String(e) };
  }
}

/** Splits `https://host[:port][/]` into the host part and whether it is plain http. */
function serverHost(server: string): [string, boolean] {
  let http: boolean;
  let rest: string;
  if (server.startsWith("https://")) [http, rest] = [false, server.slice(8)];
  else if (server.startsWith("http://")) [http, rest] = [true, server.slice(7)];
  else return bad(`\`${server}\` is not an http or https address`);
  if (rest.endsWith("/")) rest = rest.slice(0, -1);
  if (rest.includes("/")) bad("links need a server at the root of its address, without a path");
  return [authority(rest), http];
}

/** Writes the link to `link`; throws a {@link LinkError} when it cannot be written. */
export function build(link: Link): string {
  checkDepot(link.depot);
  const [host, http] = serverHost(link.server);
  const query: string[] = [];
  let tail: string;
  if (link.kind === "revision") tail = `r${link.rev}`;
  else if (link.kind === "shelf") tail = `shelf/${link.id}`;
  else {
    if (link.rev === null && link.branch === null) bad("a file link needs a revision or a branch");
    const segs = link.path.split("/").map((s) => encode(segment(encode(s))));
    if (link.branch !== null) {
      checkBranch(link.branch);
      query.push(`branch=${encode(link.branch)}`);
    }
    tail = link.rev === null ? segs.join("/") : `${segs.join("/")}@${link.rev}`;
  }
  if (http) query.push("http=1");
  const text = `lode://${host}/${link.depot}/${tail}${query.length ? `?${query.join("&")}` : ""}`;
  // Never hand out a link that would be refused, such as a branch link to a file named `r1`.
  parse(text);
  return text;
}

/** `https://host[:port]` of a server address, for telling whether two name the same server. */
export function sameServer(a: string, b: string): boolean {
  const norm = (s: string) => {
    try {
      const u = new URL(s);
      return `${u.protocol}//${u.host}`.toLowerCase();
    } catch {
      return s.replace(/\/+$/, "").toLowerCase();
    }
  };
  return norm(a) === norm(b);
}
