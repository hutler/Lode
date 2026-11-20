"""Export the small mascot from the project's full-size transparent portrait.

Run from docs/logo: python source/mascot.py lode-mascot (requires Pillow).
"""

import sys
from pathlib import Path

from PIL import Image


def export_thumbnail(stem: Path) -> None:
    with Image.open(stem.with_suffix(".png")) as portrait:
        thumbnail = portrait.convert("RGBA").resize((64, 64), Image.Resampling.LANCZOS)
        thumbnail.save(stem.with_name(stem.name + "-64.png"))


if __name__ == "__main__":
    export_thumbnail(Path(sys.argv[1]))
