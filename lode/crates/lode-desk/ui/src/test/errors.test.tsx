import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ErrorBanner } from "../components/ErrorBanner";

describe("ErrorBanner", () => {
  it("explains obliterated content", () => {
    render(<ErrorBanner error={{ code: "obliterated", message: "content abc was obliterated from this depot" }} />);
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("content abc was obliterated");
    expect(alert).toHaveTextContent("removed this content from the server for good");
    expect(alert).toHaveTextContent("obliterated");
  });

  it("explains a failed single sign-on", () => {
    render(<ErrorBanner error={{ code: "sso_failed", message: "single sign-on failed: no Lode user is linked" }} />);
    expect(screen.getByRole("alert")).toHaveTextContent("ask a server administrator to add or enable it");
  });
});
