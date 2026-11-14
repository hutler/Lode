import { useEffect, useState } from "react";

import { build, type Link } from "../links";
import { Modal } from "./Modal";

/** What a link points at, in words. */
export function linkTarget(link: Link): string {
  if (link.kind === "revision") return `r${link.rev}`;
  if (link.kind === "shelf") return `shelf ${link.id}`;
  if (link.rev !== null) return `${link.path} at r${link.rev}`;
  return `${link.path} on ${link.branch ?? "its branch"}`;
}

function hostOf(server: string): string {
  try {
    return new URL(server).host;
  } catch {
    return server;
  }
}

/** A button that copies the `lode://` link to `link`. */
export function CopyLink({ link, label, text = "Copy link" }: { link: Link; label: string; text?: string }) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  useEffect(() => {
    if (state === "idle") return;
    const t = setTimeout(() => setState("idle"), 2000);
    return () => clearTimeout(t);
  }, [state]);
  let built: string | null = null;
  let why: string | null = null;
  try {
    built = build(link);
  } catch (e) {
    why = e instanceof Error ? e.message : String(e);
  }
  const copy = async () => {
    if (!built) return;
    try {
      await navigator.clipboard.writeText(built);
      setState("copied");
    } catch {
      setState("failed");
    }
  };
  return (
    <button
      className="link copy-link"
      onClick={() => void copy()}
      disabled={!built}
      title={why ? `No link: ${why}` : undefined}
      aria-label={label}
    >
      {state === "copied" ? "Copied" : state === "failed" ? "Could not copy" : text}
    </button>
  );
}

/** A link that was refused; nothing was opened. */
export function RefusedLinkDialog({ text, why, onClose }: { text: string; why: string; onClose: () => void }) {
  return (
    <Modal title="This link cannot be opened" onClose={onClose}>
      <p className="mono small link-text">{text.length > 200 ? `${text.slice(0, 200)}…` : text}</p>
      <p>{why[0]?.toUpperCase() + why.slice(1)}. Nothing was opened.</p>
      <div className="form-actions">
        <button className="primary" onClick={onClose} autoFocus>
          OK
        </button>
      </div>
    </Modal>
  );
}

export interface Candidate {
  path: string;
  branch: string;
  open: boolean;
}

/** Several workspaces could open the link: which one. */
export function ChooseWorkspaceDialog({
  link,
  candidates,
  onPick,
  onClose,
}: {
  link: Link;
  candidates: Candidate[];
  onPick: (path: string) => void;
  onClose: () => void;
}) {
  return (
    <Modal title="Open the link in which workspace?" onClose={onClose}>
      <p>
        Several workspaces of {link.depot} on {hostOf(link.server)} can show {linkTarget(link)}.
      </p>
      <ul className="link-choices" aria-label="Workspaces">
        {candidates.map((c) => (
          <li key={c.path}>
            <button onClick={() => onPick(c.path)} aria-label={`Open in ${c.path}`}>
              <span className="mono">{c.path}</span>
              <span className="tab-branch">{c.branch}</span>
              {c.open && <span className="muted small">open</span>}
            </button>
          </li>
        ))}
      </ul>
      <div className="form-actions">
        <button onClick={onClose}>Cancel</button>
      </div>
    </Modal>
  );
}

/** No workspace can open the link: offer a checkout. */
export function NoWorkspaceDialog({
  link,
  onCheckout,
  onClose,
}: {
  link: Link;
  onCheckout: () => void;
  onClose: () => void;
}) {
  return (
    <Modal title="No workspace for this link" onClose={onClose}>
      <p>
        The link shows {linkTarget(link)} in {link.depot} on {hostOf(link.server)}, and no open or recent workspace is
        of that depot. Check it out to open the link.
      </p>
      <div className="form-actions">
        <button onClick={onClose}>Cancel</button>
        <button className="primary" onClick={onCheckout} autoFocus>
          Check out {link.depot}…
        </button>
      </div>
    </Modal>
  );
}
