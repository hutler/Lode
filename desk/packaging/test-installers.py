"""Exercise signed installer upgrades and desktop URL activation on a disposable Windows runner."""

import functools
import hashlib
import http.server
import json
import os
import subprocess
import threading
import time
import winreg
from pathlib import Path

from comtypes import COMError
from pywinauto import Desktop
from pywinauto.keyboard import send_keys
from pywinauto.uia_defines import NoPatternInterfaceError

# Windows may replace an accessibility element between discovery and use.
UI_ERRORS = (AttributeError, COMError, NoPatternInterfaceError, OSError)

root = Path(os.environ["RUNNER_TEMP"]) / "lode-acceptance"
baseline = Path(os.environ["PREVIOUS_MSI"])
target = os.environ["ACCEPTANCE_STAGE"]
version = os.environ["DESK_VERSION"]
os.environ["SLINT_BACKEND"] = "winit-software"


def wait_for(description, read, seconds=75):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        try:
            result = read()
            if result:
                return result
        except UI_ERRORS:
            pass
        time.sleep(0.4)
    raise RuntimeError("Timed out waiting for " + description)


def registered():
    with winreg.OpenKey(winreg.HKEY_CLASSES_ROOT, r"lode\shell\open\command") as key:
        cmd = winreg.QueryValue(key, None)
    return Path(cmd.split('"')[1] if cmd.startswith('"') else cmd.split(" %")[0])


def window():
    for w in Desktop(backend="uia").windows():
        if w.window_text().startswith("Lode Desk"):
            return w
    return None


def named(name, role=None):
    w = window()
    if w:
        for c in w.descendants():
            if c.element_info.name == name and (
                role is None or c.element_info.control_type == role
            ):
                return c
    return None


def contains_text(fragment):
    w = window()
    if w:
        for c in w.descendants():
            values = [c.element_info.name or ""]
            try:
                values.append(c.iface_value.CurrentValue or "")
            except UI_ERRORS:
                pass
            try:
                values.append(c.iface_text.DocumentRange.GetText(-1) or "")
            except UI_ERRORS:
                pass
            if any(fragment.lower() in value.lower() for value in values):
                return True
    return False


def button(name):
    c = wait_for(
        name, lambda: named(name, "CheckBox" if name.startswith("Pick ") else "Button")
    )
    try:
        c.invoke()
    except UI_ERRORS:
        c.click_input()


def launch():
    subprocess.Popen([str(registered().resolve())])
    return wait_for("Lode Desk window", window)


def quit_app():
    w = window()
    if w:
        w.close()
        wait_for("Desk to close", lambda: window() is None)


def install(file, op="/i"):
    r = subprocess.run(["msiexec", op, str(file), "/qn", "/norestart"], check=False)
    if r.returncode not in (0, 3010):
        raise RuntimeError(f"Windows Installer returned {r.returncode}")


def shell_links():
    os.startfile("lode://lode.example.com/game")
    w = wait_for("URL activation", window)
    button("OK")
    pid = w.process_id()
    os.startfile("lode://lode.example.com/game")
    button("OK")
    assert window().process_id() == pid
    os.startfile("lode://lode.example.com/missing/r1")
    wait_for("checkout offered for unknown depot", lambda: named("Check out missing…"))
    button("Cancel")
    print(
        "PASS registered URI launched Desk and a second URI reached the original process",
        flush=True,
    )


def exercise_workspace():
    import atexit
    import secrets
    import socket
    from urllib.request import urlopen

    binaries = Path(os.environ["CORE_PROGRAMS"])
    cli = next(binaries.rglob("lode.exe"))
    server = next(binaries.rglob("loded.exe"))
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        address = f"127.0.0.1:{s.getsockname()[1]}"
    url = "http://" + address
    password = secrets.token_hex(20)
    pw = root / "bootstrap.pw"
    pw.write_text(password)
    data = root / "server"
    out = subprocess.check_output(
        [
            server,
            "setup",
            "--yes",
            "--local",
            "--listen",
            address,
            "--data-dir",
            data,
            "--admin-password-file",
            pw,
            "--print-admin-token",
        ],
        text=True,
    )
    token_file = root / "bootstrap.token"
    token_file.write_text(
        next(line for line in out.splitlines() if line.startswith("lode_"))
    )
    proc = subprocess.Popen(
        [server, "serve", "--config", data / "loded.toml"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    atexit.register(proc.terminate)

    def online():
        with urlopen(url + "/api/v1/info", timeout=2) as response:
            return response.status == 200

    wait_for("private test server", online)

    def run(args, cwd=root, token=None, expect_fail=False):
        env = dict(os.environ, LODE_SERVER=url)
        if token:
            env["LODE_TOKEN_FILE"] = str(token)
        p = subprocess.run(
            [cli, "--insecure", "--server", url, "--json", *args],
            cwd=cwd,
            env=env,
            check=False,
            capture_output=True,
            text=True,
        )
        if expect_fail:
            assert p.returncode != 0 and "locked" in (p.stdout + p.stderr).lower()
            return
        assert p.returncode == 0, f"CLI {args[0]} failed with exit {p.returncode}"
        return json.loads(p.stdout)

    run(["admin", "depot", "create", "game"], token=token_file)
    agent = run(
        [
            "admin",
            "agent",
            "create",
            "reviewer",
            "--owner",
            "admin",
            "--grant",
            "game=write",
            "--with-token",
        ],
        token=token_file,
    )
    agent_file = root / "review.token"
    agent_file.write_text(
        agent["token"]["token"] if isinstance(agent["token"], dict) else agent["token"]
    )
    seed = root / "seed"
    run(["checkout", url + "/game", str(seed)], token=token_file)
    for file in ("first.txt", "second.txt", "scene.bin"):
        (seed / file).write_bytes(b"seed\n")
    run(["add", "first.txt", "second.txt", "scene.bin"], seed, token_file)
    run(["commit", "-m", "Seed acceptance data"], seed, token_file)

    def edit(name, value):
        c = wait_for(name, lambda: named(name, "Edit"))
        c.click_input()
        send_keys("^a")
        send_keys(value, with_spaces=True, vk_packet=True, pause=0.01)

    launch()
    button("Sign in to a server…")
    edit("Server", url)
    edit("Name", "admin")
    edit("Password", "incorrect")
    button("Sign in")
    wait_for(
        "wrong password refused",
        lambda: (
            contains_text("wrong name or password") or contains_text("unauthenticated")
        ),
    )
    edit("Password", password)
    button("Sign in")
    wait_for("valid sign-in", lambda: named("Done", "Button"))
    button("Done")
    ws = root / "desk-workspace"
    button("Check out a depot…")
    edit("Depot", url + "/game")
    edit("Folder", str(ws))
    button("Check out")
    wait_for("workspace checked out", lambda: (ws / "scene.bin").is_file())
    assert "admin" in json.dumps(run(["whoami"], ws))
    other = root / "another-workspace"
    run(["checkout", url + "/game", str(other)], token=agent_file)
    run(["lock", "scene.bin", "-m", "QA lock"], ws)
    run(
        ["lock", "scene.bin", "-m", "Second owner"], other, agent_file, expect_fail=True
    )
    for file in ("first.txt", "second.txt"):
        (ws / file).write_text("changed\n")
    wait_for(
        "both changed files",
        lambda: all(
            named("Pick " + n, "CheckBox") for n in ("first.txt", "second.txt")
        ),
    )
    for count, file in enumerate(("first.txt", "second.txt"), 1):
        label = "Commit " + str(count) + (" file" if count == 1 else " files")
        control = named("Pick " + file, "CheckBox")
        try:
            control.iface_toggle.Toggle()
        except UI_ERRORS:
            control.click_input()
        time.sleep(0.6)
        if not named(label, "Button"):
            named("Pick " + file, "CheckBox").click_input()
        wait_for(label, lambda label=label: named(label, "Button"), 12)
    message = "Packaged Desk acceptance"
    field = named("Commit message", "Edit")
    commit = lambda: named("Commit 2 files", "Button")
    try:
        field.iface_value.SetValue(message)
    except UI_ERRORS:
        pass
    if not commit().is_enabled():
        field.set_focus()
        send_keys("^a")
        send_keys(message, with_spaces=True, vk_packet=True)
    if not commit().is_enabled():
        edit("Commit message", message)
    assert commit().is_enabled(), (
        "Commit is disabled after selecting both files and entering a message"
    )
    print("PASS both files selected and commit enabled", flush=True)
    commit().invoke()

    def revision():
        rows = run(["log", "--depot", "game", "--verbose"], token=token_file)[
            "revisions"
        ]
        for row in rows:
            if "Packaged Desk acceptance" in json.dumps(row) and all(
                name in json.dumps(row) for name in ("first.txt", "second.txt")
            ):
                return row["rev"]
        return None

    rev = wait_for("committed both files", revision)
    quit_app()
    os.startfile(f"lode://{address}/game/r{rev}?http=1")
    wait_for("history after URL activation", lambda: named("History"))
    assert ws.is_dir()
    quit_app()
    print(
        "PASS sign-in refusal and success, shared CLI identity, checkout, concurrent lock refusal, commit and history link",
        flush=True,
    )
    return ws


def main():
    if os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise RuntimeError(
            "Run the installer test on a disposable GitHub-hosted runner"
        )
    root.mkdir(exist_ok=True)
    install(baseline)
    os.environ["LODE_DESK_NO_UPDATE"] = "1"
    shell_links()
    quit_app()
    launch()
    button("Check for updates now")
    wait_for(
        "updates disabled feedback",
        lambda: contains_text("turned off on this computer"),
    )
    quit_app()
    del os.environ["LODE_DESK_NO_UPDATE"]
    pkg_dir = Path(os.environ["CANDIDATE_FILES"])
    msi = next(pkg_dir.glob("*.msi"))
    setup = next(pkg_dir.glob("*-setup.exe"))
    if target == "candidate":
        serve = functools.partial(
            http.server.SimpleHTTPRequestHandler, directory=str(pkg_dir)
        )
        httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), serve)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        base = f"http://127.0.0.1:{httpd.server_port}"
        feed = json.loads((pkg_dir / "latest.json").read_text(encoding="utf-8"))
        for key, pkg in [
            ("windows-x86_64-msi", msi),
            ("windows-x86_64-nsis", setup),
            ("windows-x86_64", msi),
        ]:
            feed["platforms"][key]["url"] = base + "/" + pkg.name
        (pkg_dir / "test-feed.json").write_text(json.dumps(feed), encoding="utf-8")
        os.environ["LODE_DESK_UPDATE_FEED"] = base + "/test-feed.json"
    launch()
    button("Check for updates now")
    button("What's new")
    wait_for("release notes", lambda: named("Hide what's new", "Button"))
    old_pid = window().process_id()
    button("Install and restart")
    wait_for(
        "updated app restart",
        lambda: window() and window().process_id() != old_pid,
        150,
    )

    extracted = root / "candidate-msi"
    code = subprocess.run(
        ["msiexec", "/a", str(msi), "/qn", "TARGETDIR=" + str(extracted)], check=False
    ).returncode
    assert code in (0, 3010), code
    reference = next(extracted.rglob("lode-desk.exe"))

    def digest(file):
        with file.open("rb") as f:
            return hashlib.file_digest(f, "sha256").hexdigest()

    assert digest(reference) == digest(registered()), (
        "Installed executable differs from the signed candidate"
    )
    wait_for(f"Home to display {version}", lambda: contains_text(version))
    print(f"PASS installed update verified and restarted at {version}", flush=True)
    quit_app()
    shell_links()
    quit_app()
    workspace = exercise_workspace()
    install(msi, "/x")
    subprocess.run([str(setup), "/S"], check=True, timeout=150)
    launch()
    assert registered().is_file()
    quit_app()
    uninstaller = registered().parent / "uninstall.exe"
    subprocess.run([str(uninstaller), "/S"], check=True, timeout=150)

    def removed():
        try:
            registered()
            return False
        except FileNotFoundError:
            return True

    wait_for("uninstall to remove URI association", removed)
    assert (workspace / "first.txt").read_text().strip() == "changed"
    print("PASS NSIS install, launch and uninstall, workspace preserved", flush=True)


if __name__ == "__main__":
    main()
