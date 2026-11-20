"""Asset file names follow the team's naming rules in names.toml."""
import os
import re
import tomllib

from report import Problems, changes, present

HERE = os.path.dirname(os.path.abspath(__file__))
with open(os.path.join(HERE, "names.toml"), "rb") as fh:
    rules = tomllib.load(fh)
prefixes = rules.get("prefixes", {})
allowed = re.compile(r"^[A-Za-z0-9_]+$")

problems = Problems()
for path in present(changes()):
    folder, name = os.path.split(path)
    stem, _ = os.path.splitext(name)
    if not allowed.match(stem):
        problems.error(path, f"`{stem}` may use letters, digits and _ only")
        continue
    for prefix_folder, prefix in prefixes.items():
        if folder == prefix_folder or folder.startswith(prefix_folder + "/"):
            if not stem.startswith(prefix):
                problems.error(path, f"files in {prefix_folder} start with {prefix}")
            break
problems.finish()
