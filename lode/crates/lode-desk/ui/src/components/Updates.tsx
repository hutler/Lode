import { useCallback, useEffect, useState } from "react";

import { asOpError, backend, type OpError, type Settings, type UpdateCheck } from "../api";
import { ErrorBanner } from "./ErrorBanner";

/** How often a running app looks for a new version. */
export const DAY_MS = 24 * 60 * 60 * 1000;

type Available = Extract<UpdateCheck, { state: "available" }>;

/** Looks for a new version at start and once a day (the backend says when checks are off),
 * and offers the one it finds until the user says "Later" to that version. */
export function useUpdates() {
  const [offer, setOffer] = useState<Available | null>(null);
  const [later, setLater] = useState<string | null>(null);

  const found = useCallback((check: UpdateCheck) => {
    if (check.state === "available") setOffer(check);
  }, []);

  useEffect(() => {
    let live = true;
    const look = async () => {
      try {
        const check = await (await backend()).updateCheck(false);
        if (live) found(check);
      } catch {
        // A failed daily check stays quiet; "Check now" shows why.
      }
    };
    void look();
    const timer = setInterval(() => void look(), DAY_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [found]);

  return {
    offer: offer && offer.version !== later ? offer : null,
    found,
    later: () => setLater(offer?.version ?? null),
  };
}

/** A new version: what is new, "Install and restart" and "Later". */
export function UpdateBanner({ offer, onLater }: { offer: Available; onLater: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<OpError | null>(null);
  const install = async () => {
    setBusy(true);
    setError(null);
    try {
      // The app restarts when the update is in place.
      await (await backend()).updateInstall();
    } catch (e) {
      setError(asOpError(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className="update-banner" role="region" aria-label="Update">
      <div className="update-head">
        <strong>Lode Desk {offer.version} is available.</strong>
        <span className="muted small">You have {offer.current}.</span>
        <span className="spacer" />
        <button onClick={onLater} disabled={busy}>
          Later
        </button>
        <button className="primary" onClick={() => void install()} disabled={busy}>
          {busy ? "Installing…" : "Install and restart"}
        </button>
      </div>
      {offer.notes && (
        <details>
          <summary>What's new</summary>
          <p className="update-notes">{offer.notes}</p>
        </details>
      )}
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
    </section>
  );
}

function checkText(check: UpdateCheck): string {
  switch (check.state) {
    case "off":
      return `${check.reason[0]?.toUpperCase()}${check.reason.slice(1)}.`;
    case "unconfigured":
      return "This build does not update itself.";
    case "current":
      return `Lode Desk ${check.current} is the newest version.`;
    case "available":
      return `Lode Desk ${check.version} is available.`;
  }
}

/** The update setting and "Check now", on the Home tab. */
export function UpdateSettings({ onFound }: { onFound: (check: UpdateCheck) => void }) {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [result, setResult] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<OpError | null>(null);

  useEffect(() => {
    void backend()
      .then((b) => b.settings())
      .then(setSettings)
      .catch((e) => setError(asOpError(e)));
  }, []);

  const toggle = async (on: boolean) => {
    if (!settings) return;
    const next = { ...settings, check_updates: on };
    setSettings(next);
    try {
      await (await backend()).setSettings(next);
    } catch (e) {
      setSettings(settings);
      setError(asOpError(e));
    }
  };

  const check = async () => {
    setChecking(true);
    setError(null);
    setResult(null);
    try {
      const found = await (await backend()).updateCheck(true);
      setResult(checkText(found));
      onFound(found);
    } catch (e) {
      setError(asOpError(e));
    } finally {
      setChecking(false);
    }
  };

  return (
    <div className="update-settings">
      <label className="check-row">
        <input
          type="checkbox"
          checked={settings?.check_updates ?? true}
          disabled={!settings}
          onChange={(e) => void toggle(e.target.checked)}
        />
        Check for updates automatically
      </label>
      <button onClick={() => void check()} disabled={checking}>
        {checking ? "Checking…" : "Check for updates now"}
      </button>
      {result && (
        <p className="muted small" role="status">
          {result}
        </p>
      )}
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
    </div>
  );
}
