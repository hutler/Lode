import { useEffect, useState, type FormEvent } from "react";

import { asOpError, backend, type OpError, type SsoOffer, type WhoAmI } from "../api";
import { ErrorBanner } from "./ErrorBanner";
import { Modal } from "./Modal";

interface Props {
  initialServer?: string;
  onClose: () => void;
}

const SERVER_URL = /^https?:\/\/[^/\s]+/;

/** Signs in with a name and password, or with the company's sign-in page when the server
 * offers single sign-on. The token is stored where `lode login` keeps it, so the command line
 * is signed in too. */
export function SignInDialog({ initialServer, onClose }: Props) {
  const [server, setServer] = useState(initialServer ?? "https://");
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<OpError | null>(null);
  const [done, setDone] = useState<WhoAmI | null>(null);
  const [offer, setOffer] = useState<SsoOffer | null>(null);
  const [ssoUrl, setSsoUrl] = useState<string | null>(null);
  const [waiting, setWaiting] = useState(false);

  // Ask the server whether it offers single sign-on once the address looks complete.
  useEffect(() => {
    const target = server.trim();
    setOffer(null);
    if (!SERVER_URL.test(target)) return;
    let current = true;
    const timer = setTimeout(() => {
      void backend()
        .then((b) => b.ssoOffer(target))
        .then((o) => current && setOffer(o))
        .catch(() => current && setOffer(null));
    }, 300);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [server]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const b = await backend();
      setDone(await b.signIn(server.trim(), name.trim(), password));
      setPassword("");
    } catch (err) {
      setError(asOpError(err));
    } finally {
      setBusy(false);
    }
  };

  const signInSso = async () => {
    setBusy(true);
    setWaiting(true);
    setError(null);
    setSsoUrl(null);
    const b = await backend();
    const stop = b.onSsoUrl(setSsoUrl);
    try {
      setDone(await b.signInSso(server.trim()));
    } catch (err) {
      setError(asOpError(err));
    } finally {
      stop();
      setBusy(false);
      setWaiting(false);
    }
  };

  return (
    <Modal title="Sign in" onClose={onClose}>
      {done ? (
        <div className="form">
          <p>
            Signed in to <strong>{done.server}</strong> as <strong>{done.name}</strong>.
          </p>
          <div className="form-actions">
            <button className="primary" onClick={onClose} autoFocus>
              Done
            </button>
          </div>
        </div>
      ) : (
        <form className="form" onSubmit={submit}>
          <label>
            Server
            <input
              value={server}
              onChange={(e) => setServer(e.target.value)}
              placeholder="https://lode.example.com"
              required
              autoFocus={!initialServer}
            />
          </label>
          {offer && (
            <div className="sso">
              <button type="button" className="primary" onClick={() => void signInSso()} disabled={busy}>
                Sign in with {offer.label}
              </button>
              {waiting && (
                <p role="status">
                  Finish signing in in your browser.
                  {ssoUrl && (
                    <>
                      {" "}
                      If it did not open, open this address on this computer: <code className="sso-url">{ssoUrl}</code>
                    </>
                  )}
                </p>
              )}
              <p className="hint">
                {offer.password_login
                  ? "Or sign in with a password:"
                  : "Password sign-in is for server administrators only."}
              </p>
            </div>
          )}
          <label>
            Name
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              autoComplete="username"
              required
              autoFocus={!!initialServer}
            />
          </label>
          <label>
            Password
            <input
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              autoComplete="current-password"
              required
            />
          </label>
          {error && <ErrorBanner error={error} />}
          <div className="form-actions">
            <button type="button" onClick={onClose}>
              Cancel
            </button>
            <button className={offer ? undefined : "primary"} type="submit" disabled={busy}>
              {busy && !waiting ? "Signing in…" : "Sign in"}
            </button>
          </div>
        </form>
      )}
    </Modal>
  );
}
