# Example validation scripts

A working setup for Lode's validation scripts (user guide, "Validation scripts"; SPEC §6.8).
Copy `.lodeconfig` and `tools/` to the root of a depot, adjust the paths and rules, and
commit them (through a shelf once `shelf_only` is in force).

| Script | Checks |
| --- | --- |
| `json_valid.py` | every changed JSON file parses |
| `asset_names.py` | file names under `Content/` follow the prefixes in `names.toml` |
| `max_size.py` | textures and sounds stay under a size limit, with a warning near it |

`report.py` holds the few lines they share: reading the input and printing problems. The
scripts use the standard library only. `asset_names.py` needs Python 3.11 for `tomllib`,
hence `python = "3.11"`.

Try them in a workspace before committing:

```sh
lode validate --trust      # trust this depot's scripts on this machine, and run them
lode validate Content/     # run on the changed files under Content/
```

To have the server run them on every shelf, an administrator turns on the `validate` module
(`[modules] validate = true`, `checks = true`, and `[validate] python` in `loded.toml`).
