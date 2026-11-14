#!/usr/bin/env python3
# Copyright SimpleDevOps contributors. Licensed under Apache-2.0.
"""A stand-in for `lode` in the plugin's tests (unreal T02).

Runs under the Python that ships with Unreal Engine, or any Python 3.8+. Usage:

    fake_lode.py --scenario FILE [--log FILE] <lode arguments...>

The scenario is JSON: {"calls": [call, ...]}. The first call whose "args" pattern matches the
lode arguments answers. A pattern is a list of tokens; "?" matches any one argument, "*"
matches the rest, and any other token is a shell-style pattern for one argument ("*Hero.uasset"). `--json` and `--paths-from FILE` are taken out before matching; the paths read
from the file are appended to the arguments, so patterns see them like ordinary paths.

A call may have:
  "stdout":      JSON value printed as one document (strings "{root}" become the working folder)
  "stdout_text": text printed as is (for malformed output)
  "stderr":      text printed to standard error
  "exit":        exit code (default 0)
  "sleep":       seconds to wait first (time-out and cancel tests)
  "write":       {"after": "-o", "content": "text"} writes content to the path after that option
  "flood":       number of bytes of filler sent to stderr before the answer (pipe deadlock test)
  "after":       a pattern; the call answers only once an earlier logged call matched it, so a
                 scenario can change after, say, a lock (needs --log)

With --log, each call appends one JSON line: {"args": [...], "stdin": "...", "paths": [...]}.
An unmatched call prints a `usage` error naming the arguments and exits 2.
"""
import fnmatch
import json
import os
import sys
import time


def substitute(value, root):
    if isinstance(value, str):
        return value.replace("{root}", root)
    if isinstance(value, list):
        return [substitute(v, root) for v in value]
    if isinstance(value, dict):
        return {k: substitute(v, root) for k, v in value.items()}
    return value


def matches(pattern, args):
    for index, token in enumerate(pattern):
        if token == "*":
            return True
        if index >= len(args):
            return False
        if token != "?" and not fnmatch.fnmatchcase(args[index], token):
            return False
    return len(pattern) == len(args)


def main(argv):
    scenario_path = None
    log_path = None
    rest = []
    i = 0
    while i < len(argv):
        if argv[i] == "--scenario" and scenario_path is None:
            scenario_path = argv[i + 1]
            i += 2
        elif argv[i] == "--log" and log_path is None:
            log_path = argv[i + 1]
            i += 2
        else:
            rest = argv[i:]
            break

    args = []
    paths = []
    i = 0
    while i < len(rest):
        if rest[i] == "--json":
            i += 1
        elif rest[i] == "--paths-from":
            with open(rest[i + 1], "r", encoding="utf-8-sig") as handle:
                paths = [line.rstrip("\r\n") for line in handle if line.strip()]
            i += 2
        elif rest[i] == "--":
            args.extend(rest[i + 1:])
            break
        else:
            args.append(rest[i])
            i += 1
    args.extend(paths)

    stdin_text = ""
    if not sys.stdin.isatty():
        try:
            stdin_text = sys.stdin.read()
        except (OSError, ValueError):
            stdin_text = ""

    earlier = []
    if log_path and os.path.exists(log_path):
        with open(log_path, "r", encoding="utf-8") as handle:
            earlier = [json.loads(line)["args"] for line in handle if line.strip()]

    if log_path:
        with open(log_path, "a", encoding="utf-8") as handle:
            handle.write(json.dumps({"args": args, "stdin": stdin_text, "paths": paths}) + "\n")

    root = os.getcwd().replace("\\", "/")
    with open(scenario_path, "r", encoding="utf-8") as handle:
        scenario = json.load(handle)

    for call in scenario.get("calls", []):
        if not matches(call.get("args", []), args):
            continue
        if "after" in call and not any(matches(call["after"], old) for old in earlier):
            continue
        if call.get("sleep"):
            time.sleep(float(call["sleep"]))
        if call.get("flood"):
            filler = "x" * 1023 + "\n"
            for _ in range(int(call["flood"]) // 1024):
                sys.stderr.write(filler)
            sys.stderr.flush()
        write = call.get("write")
        if write:
            target = args[args.index(write["after"]) + 1]
            with open(target, "w", encoding="utf-8") as handle:
                handle.write(write.get("content", ""))
        if "stderr" in call:
            sys.stderr.write(substitute(call["stderr"], root))
        if "stdout_text" in call:
            sys.stdout.write(call["stdout_text"])
        elif "stdout" in call:
            sys.stdout.write(json.dumps(substitute(call["stdout"], root)) + "\n")
        sys.stdout.flush()
        return int(call.get("exit", 0))

    sys.stdout.write(json.dumps({"ok": False, "error": {"code": "usage", "message": "fake lode: no answer for: " + " ".join(args)}}) + "\n")
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
