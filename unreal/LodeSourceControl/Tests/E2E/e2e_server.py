#!/usr/bin/env python3
# Copyright SimpleDevOps contributors. Licensed under Apache-2.0.
"""Sets up a real Lode server and two workspaces for the plugin's end-to-end tests (unreal T10).

    e2e_server.py start --lode PATH --loded PATH --out FILE
    e2e_server.py stop FILE

`start` makes a temporary folder with a `loded` on a free port, a depot "game" holding a small
Unreal-like project (made with `lode init --template unreal`, so .uasset and .umap files need
locks), and two users, alice and bob, each with a checked-out workspace and a config folder of
their own. It writes FILE as JSON for the Lode.E2E tests:

    {"lode": ..., "server": ..., "pid": ..., "base": ...,
     "alice": {"root": ..., "config": ...}, "bob": {"root": ..., "config": ...}}

and leaves the server running. `stop` ends the server and removes the folder. The tests switch
users by setting LODE_CONFIG_DIR, so nothing touches your own Lode sign-in.
"""
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def lode(exe, config, cwd, *args, stdin=None):
    env = dict(os.environ)
    env["LODE_CONFIG_DIR"] = config
    for name in ("LODE_SERVER", "LODE_TOKEN_FILE", "LODE_WORKSPACE", "LODE_DEPOT"):
        env.pop(name, None)
    proc = subprocess.run([exe, *args, "--json"], input=(stdin or "").encode(), capture_output=True,
                          cwd=cwd, env=env, timeout=120)
    try:
        doc = json.loads(proc.stdout.decode("utf-8"))
    except json.JSONDecodeError:
        doc = {"ok": False, "raw": proc.stdout.decode("utf-8", "replace"), "stderr": proc.stderr.decode("utf-8", "replace")}
    if proc.returncode != 0:
        sys.exit("lode %s failed: %s" % (" ".join(args), json.dumps(doc)))
    return doc


def start(opts):
    base = os.path.realpath(tempfile.mkdtemp(prefix="lode-e2e-"))
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
    log = open(os.path.join(base, "loded.log"), "wb")
    flags = subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0
    daemon = subprocess.Popen([opts.loded, "serve", "--config", config], cwd=data, stdout=log, stderr=log, creationflags=flags)
    for _ in range(200):
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            break
        except OSError:
            time.sleep(0.1)
    else:
        daemon.kill()
        sys.exit("loded did not start; see %s" % os.path.join(base, "loded.log"))

    configs = {who: os.path.join(base, "config-" + who) for who in ("admin", "alice", "bob")}
    for folder in configs.values():
        os.makedirs(folder)
    lode(opts.lode, configs["admin"], base, "login", server, "--token-stdin", stdin=token)
    lode(opts.lode, configs["admin"], base, "admin", "depot", "create", "game", "--server", server)
    for name in ("alice", "bob"):
        lode(opts.lode, configs["admin"], base, "admin", "user", "create", name, "--password-stdin", "--server", server, stdin=name + "-password-1")
    lode(opts.lode, configs["admin"], base, "admin", "acl", "add", "allow", "write", "all", "--depot", "game", "--server", server)
    for name in ("alice", "bob"):
        lode(opts.lode, configs[name], base, "login", server, "--name", name, "--password-stdin", stdin=name + "-password-1")

    source = os.path.join(base, "import")
    files = {
        "Game.uproject": b'{"FileVersion": 3, "EngineAssociation": "5.4"}\n',
        "Content/Hero/Hero.uasset": os.urandom(3000),
        "Content/Maps/Main.umap": os.urandom(2000),
        "Config/DefaultEngine.ini": b"[/Script/Engine.RendererSettings]\nr.DefaultFeature.Bloom=True\n",
    }
    for path, content in files.items():
        os.makedirs(os.path.dirname(os.path.join(source, path)) or source, exist_ok=True)
        with open(os.path.join(source, path), "wb") as handle:
            handle.write(content)
    lode(opts.lode, configs["admin"], source, "init", server + "/game", "--template", "unreal", "-m", "Import")

    out = {"lode": os.path.abspath(opts.lode), "server": server, "pid": daemon.pid, "base": base}
    for name in ("alice", "bob"):
        root = os.path.join(base, name, "Game")
        lode(opts.lode, configs[name], base, "checkout", server + "/game", root)
        out[name] = {"root": root.replace("\\", "/"), "config": configs[name]}
    with open(opts.out, "w", encoding="utf-8") as handle:
        json.dump(out, handle, indent=1)
    print("Lode end-to-end server on %s; details in %s" % (server, opts.out))


def stop(path):
    with open(path, encoding="utf-8") as handle:
        info = json.load(handle)
    try:
        if os.name == "nt":
            subprocess.run(["taskkill", "/PID", str(info["pid"]), "/T", "/F"], capture_output=True)
        else:
            os.kill(info["pid"], signal.SIGTERM)
    except OSError:
        pass
    time.sleep(0.5)
    shutil.rmtree(info["base"], ignore_errors=True)
    os.remove(path)


def main():
    import argparse
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="action", required=True)
    up = sub.add_parser("start")
    up.add_argument("--lode", required=True)
    up.add_argument("--loded", required=True)
    up.add_argument("--out", required=True)
    down = sub.add_parser("stop")
    down.add_argument("file")
    opts = parser.parse_args()
    if opts.action == "start":
        start(opts)
    else:
        stop(opts.file)


if __name__ == "__main__":
    main()
