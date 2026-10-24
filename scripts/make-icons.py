#!/usr/bin/env python3
"""Builds the launcher's application icons from the official Zebra icon.

Source: src-tauri/icons/source/zebra-icon.png, the round Zebra gear from
https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png (938 x 938, transparent).

Steps:
  1. Compose a 1024 px icon: the gear centred at 84 % of the canvas, transparent margin, so the
     icon sits on the same grid as other desktop icons.
  2. `cargo tauri icon` turns it into every PNG size, icon.ico and icon.icns.
  3. icon.ico and icon.icns are rewritten with Pillow so the tiny renderings use a simplified
     variant (the amber disc without gear teeth and the zebra head enlarged): at those sizes
     the full gear smears into a blob while the head still reads as a zebra. In icon.ico that
     is the 16 and 24 px frames; in icon.icns it is the 16-point Retina slot (`ic11`, a 32 px
     image), the smallest slot Pillow writes (it writes `ic07` to `ic14`, no 16 or 32 px at 1x,
     which macOS scales from the Retina slots).

Needs Pillow (`pip install pillow`) and the pinned tauri-cli. Run from the repository root:

    python3 -P scripts/make-icons.py
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
ICONS = ROOT / "src-tauri" / "icons"
SOURCE = ICONS / "source" / "zebra-icon.png"

CANVAS = 1024
DIAMETER = 860
# The gear ring's colour, sampled from the source.
AMBER = (0xD3, 0x8E, 0x2D, 255)
# Sizes that get the simplified variant.
SIMPLIFIED_ICO = (16, 24)
# Pillow keys provided ICNS images by pixel size; 32 px fills `ic11`, the 16-point Retina slot.
SIMPLIFIED_ICNS = (32,)
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)


def compose(source: Image.Image) -> Image.Image:
    """The 1024 px icon: the gear, trimmed and centred, on a transparent canvas."""
    gear = source.crop(source.getbbox())
    side = max(gear.size)
    square = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    square.paste(gear, ((side - gear.size[0]) // 2, (side - gear.size[1]) // 2))
    scaled = square.resize((DIAMETER, DIAMETER), Image.LANCZOS)
    out = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    out.paste(scaled, ((CANVAS - DIAMETER) // 2, (CANVAS - DIAMETER) // 2), scaled)
    return out


def head_crop(source: Image.Image) -> Image.Image:
    """The zebra head: the bounding box of the source's black pixels, with a little room."""
    pixels = source.load()
    width, height = source.size
    xs: list[int] = []
    ys: list[int] = []
    for y in range(0, height, 2):
        for x in range(0, width, 2):
            r, g, b, a = pixels[x, y]
            if a > 200 and r < 60 and g < 60 and b < 60:
                xs.append(x)
                ys.append(y)
    pad = 40
    box = (
        max(0, min(xs) - pad),
        max(0, min(ys) - pad),
        min(width, max(xs) + 1 + pad),
        min(height, max(ys) + 1 + pad),
    )
    return source.crop(box)


def simplified(head: Image.Image, size: int) -> Image.Image:
    """The small-size variant: amber disc, white centre, the head at 78 % of the disc."""
    big = size * 8
    im = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    draw = ImageDraw.Draw(im)
    draw.ellipse((0, 0, big - 1, big - 1), fill=AMBER)
    inner = int(big * 0.86)
    offset = (big - inner) // 2
    draw.ellipse((offset, offset, offset + inner, offset + inner), fill=(255, 255, 255, 255))
    target = int(big * 0.78)
    scale = min(target / head.size[0], target / head.size[1])
    fitted = head.resize(
        (max(1, int(head.size[0] * scale)), max(1, int(head.size[1] * scale))), Image.LANCZOS
    )
    im.paste(
        fitted,
        ((big - fitted.size[0]) // 2, (big - fitted.size[1]) // 2 + int(big * 0.02)),
        fitted,
    )
    return im.resize((size, size), Image.LANCZOS)


def main() -> int:
    source = Image.open(SOURCE).convert("RGBA")
    icon = compose(source)
    head = head_crop(source)

    with tempfile.TemporaryDirectory() as tmp:
        big_path = Path(tmp) / "zebra-launcher-icon-1024.png"
        icon.save(big_path, optimize=True)
        subprocess.run(
            ["cargo", "tauri", "icon", str(big_path), "-o", str(ICONS)],
            check=True,
            cwd=ROOT,
        )
    # Desktop only: the mobile sets are not used by this app.
    for mobile in ("android", "ios"):
        shutil.rmtree(ICONS / mobile, ignore_errors=True)

    # icon.ico: every size from the 1024 px icon, the tiny ones simplified. Pillow skips any
    # size larger than the base image, so the largest frame is the base.
    frames = [
        simplified(head, size) if size in SIMPLIFIED_ICO else icon.resize((size, size), Image.LANCZOS)
        for size in ICO_SIZES
    ]
    frames[-1].save(
        ICONS / "icon.ico",
        format="ICO",
        sizes=[(size, size) for size in ICO_SIZES],
        append_images=frames[:-1],
    )

    # icon.icns: Pillow renders its slots from the 1024 px icon, except where a provided image
    # has the slot's pixel size.
    extra = [simplified(head, size) for size in SIMPLIFIED_ICNS]
    icon.save(ICONS / "icon.icns", format="ICNS", append_images=extra)

    print("icons written to", ICONS)
    return 0


if __name__ == "__main__":
    sys.exit(main())
