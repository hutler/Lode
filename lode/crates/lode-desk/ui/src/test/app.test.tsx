import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";

import { setBackend } from "../api";
import { App } from "../App";
import { serverOfTarget } from "../components/CheckoutDialog";
import { createMockBackend } from "../mock";

const SERVER = "https://lode.example.com";

describe("welcome page", () => {
  beforeEach(() => setBackend(createMockBackend({ stepMs: 0 })));

  it("lists recent workspaces and opens one in a tab", async () => {
    const user = userEvent.setup();
    render(<App />);
    const list = await screen.findByRole("list");
    expect(within(list).getAllByRole("listitem")).toHaveLength(3);
    await user.click(within(list).getByText("D:\\Tools\\pipeline"));
    expect(await screen.findByRole("heading", { name: "pipeline" })).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Open workspaces" })).toHaveTextContent("pipeline");
    // The overview counts the status result.
    expect(await screen.findByText("changed files")).toBeInTheDocument();
  });

  it("forgets a workspace without touching the folder", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: "Forget D:\\Tools\\pipeline" }));
    await waitFor(() => expect(screen.getAllByRole("listitem")).toHaveLength(2));
  });
});

describe("sign in", () => {
  it("signs in and says as whom", async () => {
    setBackend(createMockBackend({ stepMs: 0, signedIn: [] }));
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: "Sign in to a server…" }));
    const dialog = screen.getByRole("dialog", { name: "Sign in" });
    const server = within(dialog).getByLabelText("Server");
    await user.clear(server);
    await user.type(server, SERVER);
    await user.type(within(dialog).getByLabelText("Name"), "alice");
    await user.type(within(dialog).getByLabelText("Password"), "correct horse");
    await user.click(within(dialog).getByRole("button", { name: "Sign in" }));
    expect(await within(dialog).findByText(/Signed in to/)).toHaveTextContent(`Signed in to ${SERVER} as alice.`);
  });

  it("shows the server's message and the code on a wrong password", async () => {
    setBackend(createMockBackend({ stepMs: 0, signedIn: [] }));
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: "Sign in to a server…" }));
    const dialog = screen.getByRole("dialog", { name: "Sign in" });
    await user.clear(within(dialog).getByLabelText("Server"));
    await user.type(within(dialog).getByLabelText("Server"), SERVER);
    await user.type(within(dialog).getByLabelText("Name"), "alice");
    await user.type(within(dialog).getByLabelText("Password"), "nope");
    await user.click(within(dialog).getByRole("button", { name: "Sign in" }));
    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent("wrong name or password");
    expect(alert).toHaveTextContent("unauthenticated");
    expect(alert).toHaveTextContent("Sign in to the server");
  });
});

describe("check out", () => {
  async function fill(user: ReturnType<typeof userEvent.setup>, target: string, folder: string) {
    await user.click(await screen.findByRole("button", { name: "Check out a depot…" }));
    const dialog = screen.getByRole("dialog", { name: "Check out a depot" });
    const depot = within(dialog).getByLabelText(/Depot/);
    await user.clear(depot);
    await user.type(depot, target);
    await user.type(within(dialog).getByLabelText("Folder"), folder);
    await user.click(within(dialog).getByRole("button", { name: "Check out" }));
    return dialog;
  }

  it("checks out, reports progress and opens the new workspace", async () => {
    const backend = createMockBackend({ stepMs: 0, recent: [] });
    const seen: number[] = [];
    backend.onProgress((e) => seen.push(e.done));
    setBackend(backend);
    const user = userEvent.setup();
    render(<App />);
    await fill(user, `${SERVER}/pipeline`, "D:\\Work\\pipeline");
    expect(await screen.findByRole("heading", { name: "pipeline" })).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(seen.at(-1)).toBe(412);
    expect(await backend.recentWorkspaces()).toEqual([
      { path: "D:\\Work\\pipeline", depot: "pipeline", server: SERVER, branch: "main" },
    ]);
  });

  it("offers to sign in when the server asks for it", async () => {
    setBackend(createMockBackend({ stepMs: 0, signedIn: [] }));
    const user = userEvent.setup();
    render(<App />);
    const dialog = await fill(user, `${SERVER}/pipeline`, "D:\\Work\\pipeline");
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("not signed in");
    await user.click(within(dialog).getByRole("button", { name: `Sign in to ${SERVER}` }));
    const signIn = screen.getByRole("dialog", { name: "Sign in" });
    expect(within(signIn).getByLabelText("Server")).toHaveValue(SERVER);
  });

  it("shows a missing depot as an error and stays open", async () => {
    setBackend(createMockBackend({ stepMs: 0 }));
    const user = userEvent.setup();
    render(<App />);
    const dialog = await fill(user, `${SERVER}/nothing`, "D:\\Work\\x");
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("depot nothing not found");
    expect(within(dialog).getByRole("alert")).toHaveTextContent("not_found");
  });

  it("splits the server from the depot like the command line", () => {
    expect(serverOfTarget("https://lode.example.com/game")).toBe("https://lode.example.com");
    expect(serverOfTarget("https://lode.example.com/team/game/")).toBe("https://lode.example.com/team");
    expect(serverOfTarget("https://lode.example.com/")).toBeNull();
    expect(serverOfTarget("game")).toBeNull();
  });
});

describe("errors", () => {
  it("shows a failed open on the page with its code", async () => {
    const backend = createMockBackend({ stepMs: 0 });
    backend.pickFolder = async () => "C:\\Not\\A\\Workspace";
    setBackend(backend);
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: "Open a workspace folder…" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("is not in a lode workspace");
    expect(alert).toHaveTextContent("not_a_workspace");
    await user.click(within(alert).getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });
});
