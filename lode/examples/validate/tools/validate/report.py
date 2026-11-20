"""Shared helpers for the example validation scripts.

A script reads one JSON object from standard input, prints one JSON line per problem and
exits 0 (passed) or 1 (failed). Anything else, including an uncaught exception, tells Lode
the check itself broke.
"""
import json
import sys


def changes():
    """The run's input: {"version", "phase", "depot", "branch", "files": [...]}."""
    return json.load(sys.stdin)


def present(data):
    """Paths of the changed files that exist after the change (added, edited or moved)."""
    return [f["path"] for f in data["files"] if f["action"] != "delete"]


class Problems:
    def __init__(self):
        self.errors = 0

    def error(self, path, message, line=None):
        self.errors += 1
        self._emit("error", path, message, line)

    def warning(self, path, message, line=None):
        self._emit("warning", path, message, line)

    def _emit(self, level, path, message, line):
        item = {"path": path, "level": level, "message": message}
        if line is not None:
            item["line"] = line
        print(json.dumps(item), flush=True)

    def finish(self):
        sys.exit(1 if self.errors else 0)
