"""Textures and sounds stay under a size limit; files close to it get a warning."""
import os

from report import Problems, changes, present

LIMITS = {".png": 8 * 1024 * 1024, ".wav": 32 * 1024 * 1024}
WARN_AT = 0.8

problems = Problems()
for path in present(changes()):
    limit = LIMITS.get(os.path.splitext(path)[1].lower())
    if limit is None:
        continue
    size = os.path.getsize(path)
    mib = f"{size / (1024 * 1024):.1f} MiB"
    if size > limit:
        problems.error(path, f"{mib}, over the {limit // (1024 * 1024)} MiB limit")
    elif size > limit * WARN_AT:
        problems.warning(path, f"{mib}, close to the {limit // (1024 * 1024)} MiB limit")
problems.finish()
