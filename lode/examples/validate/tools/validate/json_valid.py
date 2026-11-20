"""Every changed JSON file must parse."""
import json

from report import Problems, changes, present

problems = Problems()
for path in present(changes()):
    try:
        with open(path, encoding="utf-8") as fh:
            json.load(fh)
    except UnicodeDecodeError:
        problems.error(path, "not UTF-8 text")
    except json.JSONDecodeError as e:
        problems.error(path, e.msg, e.lineno)
problems.finish()
