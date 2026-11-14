import type { ProgressEvent } from "../api";

/** `12.3 MB` style sizes, like the command line prints. */
export function humanBytes(n: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let u = 0;
  while (v >= 1000 && u < units.length - 1) {
    v /= 1000;
    u += 1;
  }
  return u === 0 ? `${n} B` : `${v.toFixed(1)} ${units[u]}`;
}

/** A labelled bar for one running counter. */
export function ProgressBar({ event }: { event: ProgressEvent }) {
  const pct = event.total > 0 ? Math.min(100, (event.done / event.total) * 100) : 0;
  return (
    <div className="progress">
      <div className="progress-text">
        <span>
          {event.label} {event.done.toLocaleString("en-US")} of {event.total.toLocaleString("en-US")}
        </span>
        <span className="muted">{humanBytes(event.bytes)}</span>
      </div>
      <div
        className="progress-track"
        role="progressbar"
        aria-label={event.label}
        aria-valuemin={0}
        aria-valuemax={event.total}
        aria-valuenow={event.done}
      >
        <div className="progress-fill" style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}
