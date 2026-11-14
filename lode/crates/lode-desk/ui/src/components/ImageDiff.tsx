import { useEffect, useState, type ReactNode } from "react";

import { asOpError, backend, type ImageSide, type OpError } from "../api";
import { ErrorBanner } from "./ErrorBanner";
import { humanBytes } from "./ProgressBar";

export type ImageMode = "side" | "swipe" | "onion";

const MODE_KEY = "lode-desk.image-mode";
/** Images larger than this are compared by size only (as the app reads them into memory). */
export const IMAGE_LIMIT = 64 * 1024 * 1024;

function savedMode(): ImageMode {
  try {
    const m = localStorage.getItem(MODE_KEY);
    return m === "swipe" || m === "onion" ? m : "side";
  } catch {
    return "side";
  }
}

function saveMode(m: ImageMode) {
  try {
    localStorage.setItem(MODE_KEY, m);
  } catch {
    // Not kept between runs; nothing else depends on it.
  }
}

interface Props {
  workspace: string;
  path: string;
  /** The revision that changed the file; the working copy when null. */
  rev: number | null;
  /** What to show instead when the images cannot be shown, with the reason. */
  fallback: (why: string) => ReactNode;
}

type Dims = { w: number; h: number };

/** Before and after of an image: side by side, a swipe between them, or one over the other. */
export function ImageDiff({ workspace, path, rev, fallback }: Props) {
  const [sides, setSides] = useState<{ old: ImageSide | null; new: ImageSide | null } | null>(null);
  const [error, setError] = useState<OpError | null>(null);
  const [failed, setFailed] = useState(false);
  const [dims, setDims] = useState<{ old?: Dims; new?: Dims }>({});
  const [mode, setMode] = useState<ImageMode>(savedMode);
  const [pos, setPos] = useState(50);
  const [opacity, setOpacity] = useState(50);

  useEffect(() => {
    let live = true;
    const urls: string[] = [];
    setSides(null);
    setFailed(false);
    setDims({});
    void (async () => {
      try {
        const b = await backend();
        const [o, n] = await Promise.all([b.imageSide(workspace, path, rev, "old"), b.imageSide(workspace, path, rev, "new")]);
        for (const s of [o, n]) if (s?.url?.startsWith("blob:")) urls.push(s.url);
        if (live) setSides({ old: o, new: n });
      } catch (e) {
        if (live) setError(asOpError(e));
      }
    })();
    return () => {
      live = false;
      urls.forEach((u) => URL.revokeObjectURL(u));
    };
  }, [workspace, path, rev]);

  if (error) return <ErrorBanner error={error} />;
  if (!sides) return <p className="muted small image-loading">Loading images…</p>;
  if ([sides.old, sides.new].some((s) => s && (s.url === null || s.size > IMAGE_LIMIT))) {
    return <>{fallback("Images over 64 MB are compared by size only.")}</>;
  }
  if (failed) return <>{fallback("The app cannot show this image format, so it is compared by size only.")}</>;

  const both = !!sides.old && !!sides.new;
  const shown: ImageMode = both ? mode : "side";
  const choose = (m: ImageMode) => {
    setMode(m);
    saveMode(m);
  };
  const loaded = (which: "old" | "new") => (e: React.SyntheticEvent<HTMLImageElement>) => {
    const img = e.currentTarget;
    setDims((d) => ({ ...d, [which]: { w: img.naturalWidth, h: img.naturalHeight } }));
  };
  const caption = (label: string, s: ImageSide, d: Dims | undefined) => (
    <figcaption>
      <strong>{label}</strong>
      {d && d.w > 0 && (
        <span className="mono">
          {d.w} × {d.h}
        </span>
      )}
      <span>{humanBytes(s.size)}</span>
    </figcaption>
  );
  const img = (which: "old" | "new", s: ImageSide, extra?: React.CSSProperties) => (
    <img
      src={s.url ?? undefined}
      alt={which === "old" ? "Before" : "After"}
      onLoad={loaded(which)}
      onError={() => setFailed(true)}
      style={extra}
      draggable={false}
    />
  );
  // The stage keeps the shape of the larger image, so both overlay at the same scale.
  const big = [dims.old, dims.new].reduce<Dims | undefined>(
    (a, d) => (d && d.w > 0 && (!a || d.w * d.h > a.w * a.h) ? d : a),
    undefined,
  );

  return (
    <div className="image-diff">
      <div className="image-toolbar">
        <div className="segmented" role="group" aria-label="Image comparison">
          <button aria-pressed={shown === "side"} onClick={() => choose("side")}>
            Side by side
          </button>
          <button aria-pressed={shown === "swipe"} onClick={() => choose("swipe")} disabled={!both}>
            Swipe
          </button>
          <button aria-pressed={shown === "onion"} onClick={() => choose("onion")} disabled={!both}>
            Onion skin
          </button>
        </div>
        {shown === "swipe" && (
          <label className="image-slider">
            <input type="range" min={0} max={100} value={pos} aria-label="Swipe position" onChange={(e) => setPos(Number(e.target.value))} />
            <span className="muted small">Before left of the line, after right of it</span>
          </label>
        )}
        {shown === "onion" && (
          <label className="image-slider">
            Before
            <input
              type="range"
              min={0}
              max={100}
              value={opacity}
              aria-label="Opacity of after"
              onChange={(e) => setOpacity(Number(e.target.value))}
            />
            After
          </label>
        )}
        {!both && <span className="muted small">{sides.old ? "The file was deleted." : "The file is new."}</span>}
      </div>
      {shown === "side" ? (
        <div className="image-pair">
          {sides.old && (
            <figure className="image-panel">
              <div className="checker">{img("old", sides.old)}</div>
              {caption("Before", sides.old, dims.old)}
            </figure>
          )}
          {sides.new && (
            <figure className="image-panel">
              <div className="checker">{img("new", sides.new)}</div>
              {caption("After", sides.new, dims.new)}
            </figure>
          )}
        </div>
      ) : (
        <figure className="image-panel overlay">
          <div
            className="checker image-stage"
            style={big ? { aspectRatio: `${big.w} / ${big.h}` } : undefined}
            data-testid="image-stage"
          >
            {img("old", sides.old!)}
            {shown === "swipe"
              ? img("new", sides.new!, { clipPath: `inset(0 0 0 ${pos}%)` })
              : img("new", sides.new!, { opacity: opacity / 100 })}
            {shown === "swipe" && <div className="swipe-line" style={{ left: `${pos}%` }} aria-hidden="true" />}
          </div>
          <div className="image-captions">
            {caption("Before", sides.old!, dims.old)}
            {caption("After", sides.new!, dims.new)}
          </div>
        </figure>
      )}
    </div>
  );
}
