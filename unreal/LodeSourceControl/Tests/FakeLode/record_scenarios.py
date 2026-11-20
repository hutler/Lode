#!/usr/bin/env python3
# Copyright SimpleDevOps contributors. Licensed under Apache-2.0.
"""Records real `lode` answers into the fake's scenario files (unreal T10: the fake is checked
against the real JSON).

    record_scenarios.py --lode PATH --loded PATH [--out scenarios]

Starts a throwaway `loded` on a free port in a temporary folder, makes two users and a small
Unreal-like project, runs every command the plugin uses, and writes scenarios/workspace.json.
Workspace paths in the answers become "{root}", which the fake turns back into its working
folder. Run it after a Lode release and commit the changed scenario if the tests still pass.
"""
import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Lode:
    def __init__(self, exe, home, cwd, server):
        self.exe, self.home, self.cwd, self.server = exe, home, cwd, server

    def env(self):
        env = dict(os.environ)
        env["HOME"] = self.home
        env["USERPROFILE"] = self.home
        env["APPDATA"] = os.path.join(self.home, "AppData", "Roaming")
        env["LOCALAPPDATA"] = os.path.join(self.home, "AppData", "Local")
        env["XDG_CONFIG_HOME"] = os.path.join(self.home, ".config")
        env.pop("LODE_SERVER", None)
        env.pop("LODE_TOKEN_FILE", None)
        return env

    def run(self, *args, stdin=None, cwd=None):
        proc = subprocess.run([self.exe, *args, "--json"], input=(stdin or "").encode(), capture_output=True,
                              cwd=cwd or self.cwd, env=self.env(), timeout=120)
        out = proc.stdout.decode("utf-8").strip()
        try:
            doc = json.loads(out)
        except json.JSONDecodeError:
            sys.exit("lode %s printed no JSON: %r %r" % (" ".join(args), out, proc.stderr))
        return proc.returncode, doc


def rooted(value, root):
    text = json.dumps(value)
    for form in {root, root.replace("\\", "/"), root.replace("\\", "\\\\")}:
        text = text.replace(form, "{root}")
    return json.loads(text)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--lode", required=True)
    parser.add_argument("--loded", required=True)
    parser.add_argument("--out", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "scenarios"))
    opts = parser.parse_args()

    base = tempfile.mkdtemp(prefix="lode-record-")
    data = os.path.join(base, "data")
    port = free_port()
    server = "http://127.0.0.1:%d" % port
    init = subprocess.run([opts.loded, "init", data], capture_output=True, text=True, check=True)
    token = [line.strip() for line in init.stdout.splitlines() + init.stderr.splitlines() if line.strip().startswith("lode_")][0]
    config = os.path.join(data, "loded.toml")
    with open(config, encoding="utf-8") as handle:
        text = handle.read().replace('listen = "127.0.0.1:7480"', 'listen = "127.0.0.1:%d"' % port)
    with open(config, "w", encoding="utf-8") as handle:
        handle.write(text)
    daemon = subprocess.Popen([opts.loded, "serve", "--config", config], cwd=data, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
                break
            except OSError:
                time.sleep(0.1)

        project = os.path.join(base, "Game")
        bobws = os.path.join(base, "BobGame")
        admin = Lode(opts.lode, os.path.join(base, "home-admin"), base, server)
        alice = Lode(opts.lode, os.path.join(base, "home-alice"), project, server)
        bob = Lode(opts.lode, os.path.join(base, "home-bob"), bobws, server)
        for who in (admin, alice, bob):
            os.makedirs(who.home, exist_ok=True)
        os.makedirs(project, exist_ok=True)
        admin.run("login", server, "--token-stdin", stdin=token)
        admin.run("admin", "depot", "create", "game", "--server", server)
        for name in ("alice", "bob"):
            admin.run("admin", "user", "create", name, "--password-stdin", "--server", server, stdin=name + "-password-1")
        admin.run("admin", "acl", "add", "allow", "write", "all", "--depot", "game", "--server", server)
        alice.run("login", server, "--name", "alice", "--password-stdin", stdin="alice-password-1")
        bob.run("login", server, "--name", "bob", "--password-stdin", stdin="bob-password-1", cwd=base)

        source = os.path.join(base, "import")
        for folder in ("Content/Hero", "Content/Maps", "Config", "Saved"):
            os.makedirs(os.path.join(source, folder), exist_ok=True)
        files = {"Game.uproject": b'{"FileVersion":3}\n', "Content/Hero/Hero.uasset": os.urandom(3000),
                 "Content/Maps/Main.umap": os.urandom(2000), "Config/DefaultEngine.ini": b"[/Script/Engine]\n"}
        for path, content in files.items():
            with open(os.path.join(source, path), "wb") as handle:
                handle.write(content)
        code, doc = admin.run("init", server + "/game", "--template", "unreal", "-m", "Import", cwd=source)
        if code != 0:
            sys.exit("init failed: %s" % json.dumps(doc))
        shutil.rmtree(project, ignore_errors=True)
        for who, folder in ((alice, project), (bob, bobws)):
            code, doc = who.run("checkout", server + "/game", folder, cwd=base)
            if code != 0:
                sys.exit("checkout failed: %s" % json.dumps(doc))
        os.makedirs(os.path.join(project, "Saved"), exist_ok=True)
        with open(os.path.join(project, "Saved", "x.log"), "w", encoding="utf-8") as handle:
            handle.write("log\n")

        calls = []

        def record(who, pattern, *args, stdin=None):
            code, doc = who.run(*args, stdin=stdin)
            calls.append({"args": pattern, "exit": code, "stdout": rooted(doc, who.cwd)})
            return doc

        hero = "Content/Hero/Hero.uasset"
        # Bob locks Hero, so Alice sees it locked by someone else.
        bob.run("lock", hero, "-m", "lighting")
        record(alice, ["--version"], "--version")
        record(alice, ["whoami"], "whoami")
        record(alice, ["log", "-n", "1"], "log", "-n", "1")
        record(alice, ["status", "--unchanged", "--remote", "*"], "status", "--unchanged", "--remote", "Content", "Game.uproject", "Saved/x.log")
        record(alice, ["status", "--unchanged", "*"], "status", "--unchanged", "Content", "Game.uproject", "Saved/x.log")
        record(alice, ["lock", "*"], "lock", hero)  # refused: bob holds it
        calls[-1]["args"] = ["lock", "Content/Hero/Hero.uasset"]
        bob.run("unlock", hero)
        record(alice, ["lock", "*"], "lock", "Content/Maps/Main.umap")
        record(alice, ["locks"], "locks")
        # Alice edits the map she locked and submits it.
        with open(os.path.join(project, "Content/Maps/Main.umap"), "ab") as handle:
            handle.write(b"edit")
        record(alice, ["status"], "status")
        msg = os.path.join(base, "msg.txt")
        with open(msg, "w", encoding="utf-8") as handle:
            handle.write("Brighter sky\n\nMore light in the main map.")
        record(alice, ["commit", "-F", "?", "*"], "commit", "-F", msg, "Content/Maps/Main.umap")
        record(alice, ["log", "?", "-v", "--follow", "-n", "?"], "log", "Content/Maps/Main.umap", "-v", "--follow", "-n", "200")
        record(alice, ["cat", "?", "-o", "?"], "cat", "Content/Maps/Main.umap@2", "-o", os.path.join(base, "cat.out"))
        calls[-1]["write"] = {"after": "-o", "content": "map at r2"}  # the fake writes the file lode would
        # Changelists and a shelf.
        with open(os.path.join(project, "Config/DefaultEngine.ini"), "a", encoding="utf-8") as handle:
            handle.write("bSmooth=True\n")
        alice.run("changelist", "new", "Art", "-m", "Art pass")
        alice.run("changelist", "move", "Art", "Config/DefaultEngine.ini")
        record(alice, ["changelist", "list"], "changelist", "list")
        shelf = record(alice, ["shelve", "-c", "?", "--keep", "*"], "shelve", "-c", "Art", "--keep", "-m", "Art pass")
        if "shelf" not in shelf:
            sys.exit("shelve failed: %s" % json.dumps(shelf))
        record(alice, ["shelves", "--state", "open"], "shelves", "--state", "open")
        record(alice, ["shelf", "show", "?"], "shelf", "show", str(shelf["shelf"]))
        record(bob, ["unshelve", "?"], "unshelve", str(shelf["shelf"]))
        # Bob is behind: update brings Alice's map.
        record(bob, ["update"], "update")

        os.makedirs(opts.out, exist_ok=True)
        target = os.path.join(opts.out, "workspace.json")
        with open(target, "w", encoding="utf-8", newline="\n") as handle:
            json.dump({"recorded_with": alice.run("--version")[1].get("version"), "calls": calls}, handle, indent=1, sort_keys=True)
            handle.write("\n")
        print("wrote", target, "with", len(calls), "answers")
    finally:
        daemon.terminate()
        daemon.wait(10)
        shutil.rmtree(base, ignore_errors=True)


if __name__ == "__main__":
    main()
