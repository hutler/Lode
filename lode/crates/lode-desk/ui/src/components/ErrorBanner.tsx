import { createContext, useContext } from "react";

import type { OpError } from "../api";

/** Finishes the open workspace's stopped update or switch; null outside a workspace. */
export const FinishUpdateContext = createContext<(() => void) | null>(null);

/** What to do next, for errors where the message alone leaves the user stuck. */
const NEXT_STEP: Record<string, string> = {
  unauthenticated: "Sign in to the server from the Home tab, then try again.",
  permission_denied: "Ask a depot admin for access.",
  workspace_in_use: "Another lode command is using this workspace; try again when it has finished.",
  not_a_workspace: "Pick the folder you checked out into, or check out the depot again.",
  checkout_incomplete: "Check out into the same folder again to finish it.",
  update_incomplete: "An update or branch switch stopped before it finished. Finish it, then try again.",
  network: "Check the server address and your connection.",
  sso_failed:
    "The company sign-in did not finish. If the message names your account, ask a server administrator to add or enable it; otherwise sign in again.",
  obliterated: "An administrator removed this content from the server for good. Other versions of the file still open.",
};

/** An operation's error: its message, its code, and the next step when there is one. */
export function ErrorBanner({ error, onDismiss }: { error: OpError; onDismiss?: () => void }) {
  const next = NEXT_STEP[error.code];
  const finish = useContext(FinishUpdateContext);
  return (
    <div className="error-banner" role="alert">
      <div>
        <div className="error-message">{error.message}</div>
        {next && <div className="error-next">{next}</div>}
        <div className="error-code">{error.code}</div>
        {error.code === "update_incomplete" && finish && (
          <button className="error-action" onClick={finish}>
            Finish the update
          </button>
        )}
      </div>
      {onDismiss && (
        <button className="icon-button" aria-label="Dismiss" onClick={onDismiss}>
          ×
        </button>
      )}
    </div>
  );
}
