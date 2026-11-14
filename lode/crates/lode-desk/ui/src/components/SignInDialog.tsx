import { useState, type FormEvent } from "react";

import { asOpError, backend, type OpError, type WhoAmI } from "../api";
import { ErrorBanner } from "./ErrorBanner";
import { Modal } from "./Modal";

interface Props {
  initialServer?: string;
  onClose: () => void;
}

/** Signs in with a name and password. The token is stored where `lode login` keeps it, so the
 * command line is signed in too. */
export function SignInDialog({ initialServer, onClose }: Props) {
  const [server, setServer] = useState(initialServer ?? "https://");
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<OpError | null>(null);
  const [done, setDone] = useState<WhoAmI | null>(null);

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
            <input value={server} onChange={(e) => setServer(e.target.value)} placeholder="https://lode.example.com" required autoFocus={!initialServer} />
          </label>
          <label>
            Name
            <input value={name} onChange={(e) => setName(e.target.value)} autoComplete="username" required autoFocus={!!initialServer} />
          </label>
          <label>
            Password
            <input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" required />
          </label>
          {error && <ErrorBanner error={error} />}
          <div className="form-actions">
            <button type="button" onClick={onClose}>
              Cancel
            </button>
            <button className="primary" type="submit" disabled={busy}>
              {busy ? "Signing in…" : "Sign in"}
            </button>
          </div>
        </form>
      )}
    </Modal>
  );
}
