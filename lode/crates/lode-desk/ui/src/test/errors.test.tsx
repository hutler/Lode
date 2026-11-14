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
});
