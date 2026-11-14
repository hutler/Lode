import { Fragment, useEffect, useMemo, useState } from "react";

import { asOpError, backend, imageType, type FileDiff, type OpError, type WorkspaceInfo } from "../api";
import { PLAIN_LIMIT, parsePatch, splitRows, type DiffLine, type Hunk, type Segment } from "../diff";
import { ErrorBanner } from "./ErrorBanner";
import { IMAGE_LIMIT, ImageDiff } from "./ImageDiff";
import { humanBytes } from "./ProgressBar";

/** What to compare: a working file against its base, or a file as one revision changed it. */
export type DiffSource = { kind: "working"; path: string } | { kind: "revision"; rev: number; path: string };

export type Layout = "unified" | "split";

/** Files above this size are not diffed as text by Lode (`lode diff` reports sizes only). */
const TEXT_LIMIT = 16 * 1024 * 1024;
const LAYOUT_KEY = "lode-desk.diff-layout";

export function savedLayout(): Layout {
  try {
    return localStorage.getItem(LAYOUT_KEY) === "split" ? "split" : "unified";
  } catch {
    return "unified";
  }
}

export function saveLayout(l: Layout) {
  try {
    localStorage.setItem(LAYOUT_KEY, l);
  } catch {
    // Not kept between runs; nothing else depends on it.
  }
}

interface Props {
  info: WorkspaceInfo;
  source: DiffSource;
  onBack: () => void;
  backLabel: string;
}

/** The diff of one file, in the unified or the side-by-side layout. */
export function DiffView({ info, source, onBack, backLabel }: Props) {
  const [diff, setDiff] = useState<FileDiff | null>(null);
  const [none, setNone] = useState(false);
  const [error, setError] = useState<OpError | null>(null);
  const [layout, setLayout] = useState<Layout>(savedLayout);
  const rev = source.kind === "revision" ? source.rev : null;

  useEffect(() => {
    let live = true;
    setDiff(null);
    setNone(false);
    void (async () => {
      try {
        const b = await backend();
        const d = rev === null ? await b.diff(info.root, source.path) : await b.diffRevision(info.root, rev, source.path);
        if (!live) return;
        setDiff(d);
        setNone(d === null);
        setError(null);
      } catch (e) {
        if (live) setError(asOpError(e));
      }
    })();
    return () => {
      live = false;
    };
  }, [info.root, source.path, rev]);

  const choose = (l: Layout) => {
    setLayout(l);
    saveLayout(l);
  };
  const name = source.path.slice(source.path.lastIndexOf("/") + 1);

  return (
    <section className="diff-view" aria-label="Diff">
      <header className="changes-head">
        <button className="refresh back" onClick={onBack}>
          ← {backLabel}
        </button>
        <h1>
          <span className="mono">{name}</span>
        </h1>
        <span className="muted small mono diff-where" title={source.path}>
          {diff?.old_path && diff.old_path !== diff.path ? `${diff.old_path} → ` : ""}
          {source.path}
          {rev !== null ? ` in r${rev}` : " (working copy)"}
        </span>
        {diff && !diff.binary && (
          <span className="diff-stat">
            <span className="ins">+{diff.insertions}</span> <span className="del">−{diff.deletions}</span>
          </span>
        )}
        {!diff?.binary && (
          <div className="segmented" role="group" aria-label="Layout">
            <button aria-pressed={layout === "unified"} onClick={() => choose("unified")}>
              Unified
            </button>
            <button aria-pressed={layout === "split"} onClick={() => choose("split")}>
              Side by side
            </button>
          </div>
        )}
      </header>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      <div className="diff-body">
        {none && <p className="empty">This file has no changes.</p>}
        {diff && <FileDiffBody diff={diff} layout={layout} image={{ workspace: info.root, rev }} />}
      </div>
    </section>
  );
}

function extension(path: string): string {
  const name = path.slice(path.lastIndexOf("/") + 1);
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1) : "";
}

/** The note shown for a binary file: its sizes and mode, and why there is no diff. */
function BinaryNote({ diff, why }: { diff: FileDiff; why?: string }) {
  const ext = extension(diff.path);
  const size = (n: number | null) => (n === null ? "none" : humanBytes(n));
  const tooBig = (diff.old_size ?? 0) > TEXT_LIMIT || (diff.new_size ?? 0) > TEXT_LIMIT;
  return (
    <div className="diff-note" role="note">
      <div className="diff-note-title">
        {why && imageType(diff.path) ? "Image" : tooBig ? "Too large to compare as text" : "Binary file"}
        {ext && <span className="tag">.{ext}</span>}
      </div>
      <dl className="facts compact">
        <dt>Before</dt>
        <dd>{size(diff.old_size)}</dd>
        <dt>After</dt>
        <dd>{size(diff.new_size)}</dd>
        {diff.old_mode !== diff.new_mode && diff.old_mode && diff.new_mode && (
          <>
            <dt>Mode</dt>
            <dd className="mono">
              {diff.old_mode} → {diff.new_mode}
            </dd>
          </>
        )}
      </dl>
      <p className="muted small">
        {why ?? (tooBig ? "Lode compares files over 16 MB by size only." : "Binary content is not shown as a diff.")}
      </p>
    </div>
  );
}

/** A diff's content: an image diff for images when `image` says where they come from, a note
 * for other binary files and changes too big to lay out, the hunks otherwise. */
export function FileDiffBody({
  diff,
  layout,
  image,
}: {
  diff: FileDiff;
  layout: Layout;
  /** Where to read images from: the working copy (`rev` null) or a revision. */
  image?: { workspace: string; rev: number | null };
}) {
  const patch = diff.patch ?? "";
  const large = patch.length > PLAIN_LIMIT;
  const hunks = useMemo(() => (diff.binary || large ? [] : parsePatch(patch)), [diff.binary, large, patch]);

  if (diff.binary) {
    if (image && imageType(diff.path)) {
      if ((diff.old_size ?? 0) > IMAGE_LIMIT || (diff.new_size ?? 0) > IMAGE_LIMIT) {
        return <BinaryNote diff={diff} why="Images over 64 MB are compared by size only." />;
      }
      return (
        <ImageDiff
          workspace={image.workspace}
          path={diff.path}
          rev={image.rev}
          fallback={(why) => <BinaryNote diff={diff} why={why} />}
        />
      );
    }
    return <BinaryNote diff={diff} />;
  }
  if (large) {
    return (
      <>
        <p className="diff-plain-note" role="note">
          This diff is large ({humanBytes(patch.length)}), so it is shown as plain text, without line numbers or
          highlights.
        </p>
        <pre className="diff-plain mono">{patch}</pre>
      </>
    );
  }
  if (hunks.length === 0) {
    return (
      <p className="empty">
        {diff.old_mode !== diff.new_mode && diff.old_mode && diff.new_mode
          ? `Only the mode changed: ${diff.old_mode} → ${diff.new_mode}.`
          : "The content is the same."}
      </p>
    );
  }
  return layout === "split" ? <SplitTable hunks={hunks} /> : <UnifiedTable hunks={hunks} />;
}

function Text({ line }: { line: DiffLine }) {
  if (!line.words) return <>{line.text || " "}</>;
  return (
    <>
      {line.words.map((s: Segment, i) =>
        s.changed ? (
          <mark key={i} className="word">
            {s.text}
          </mark>
        ) : (
          <Fragment key={i}>{s.text}</Fragment>
        ),
      )}
    </>
  );
}

const SIGN = { context: " ", add: "+", delete: "-" } as const;

function UnifiedTable({ hunks }: { hunks: Hunk[] }) {
  return (
    <table className="diff-table unified" aria-label="Unified diff">
      <tbody>
        {hunks.map((h, i) => (
          <Fragment key={i}>
            <tr className="hunk">
              <td colSpan={4} className="mono">
                {h.header}
              </td>
            </tr>
            {h.lines.map((l, j) => (
              <tr key={j} className={`line ${l.kind}`}>
                <td className="num mono">{l.old ?? ""}</td>
                <td className="num mono">{l.new ?? ""}</td>
                <td className="sign mono">{SIGN[l.kind]}</td>
                <td className="code mono">
                  <Text line={l} />
                </td>
              </tr>
            ))}
          </Fragment>
        ))}
      </tbody>
    </table>
  );
}

function SplitTable({ hunks }: { hunks: Hunk[] }) {
  return (
    <table className="diff-table split" aria-label="Side-by-side diff">
      <colgroup>
        <col className="num-col" />
        <col />
        <col className="num-col" />
        <col />
      </colgroup>
      <tbody>
        {hunks.map((h, i) => (
          <Fragment key={i}>
            <tr className="hunk">
              <td colSpan={4} className="mono">
                {h.header}
              </td>
            </tr>
            {splitRows(h).map((r, j) => (
              <tr key={j} className="line">
                <td className={`num mono ${r.left?.kind ?? "empty"}`}>{r.left?.old ?? ""}</td>
                <td className={`code mono ${r.left?.kind ?? "empty"}`}>{r.left && <Text line={r.left} />}</td>
                <td className={`num mono ${r.right?.kind ?? "empty"}`}>{r.right?.new ?? ""}</td>
                <td className={`code mono ${r.right?.kind ?? "empty"}`}>{r.right && <Text line={r.right} />}</td>
              </tr>
            ))}
          </Fragment>
        ))}
      </tbody>
    </table>
  );
}
