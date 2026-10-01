#!/usr/bin/env python3
"""Crop the supplied scroll/quill once; regenerate faithful launcher bitmaps."""

import argparse
import io
import math
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
MASTER = HERE / "launcher.png"
RES = HERE.parent / "app/src/main/res"
CROP = (215, 215, 1040, 1040)
DENSITIES = {"mdpi": 1, "hdpi": 1.5, "xhdpi": 2, "xxhdpi": 3, "xxxhdpi": 4}


def png(image):
    buffer = io.BytesIO()
    image.save(buffer, "PNG", optimize=True)
    return buffer.getvalue()


def images(master, scale):
    size = round(48 * scale)
    square = master.resize((size, size), Image.Resampling.LANCZOS)
    circular = Image.new("RGBA", (size, size), "#F5F7FA")
    artwork = master.resize((round(size * .76),) * 2, Image.Resampling.LANCZOS)
    circular.paste(artwork, ((size - artwork.width) // 2,) * 2)
    mask = Image.new("L", (size, size))
    ImageDraw.Draw(mask).ellipse((0, 0, size - 1, size - 1), fill=255)
    circular.putalpha(mask)
    # Android's 108dp layer is masked/panned by the launcher. Keep every art tip
    # inside the central 66dp safe circle, not just its rectangular bounding box.
    layer = Image.new("RGBA", (round(108 * scale),) * 2)
    artwork = master.resize((round(52 * scale),) * 2, Image.Resampling.LANCZOS)
    layer.paste(artwork, ((layer.width - artwork.width) // 2,) * 2)
    return {"ic_launcher.png": square, "ic_launcher_round.png": circular,
            "ic_launcher_foreground.png": layer}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, help="original supplied 1254px PNG")
    parser.add_argument("--check", action="store_true", help="verify without writes")
    args = parser.parse_args()
    if args.source:
        if args.check:
            parser.error("--source and --check are separate operations")
        with Image.open(args.source) as image:
            if image.size != (1254, 1254):
                parser.error("expected the supplied 1254 x 1254 image")
            MASTER.write_bytes(png(image.convert("RGB").crop(CROP)))
    with Image.open(MASTER) as image:
        master = image.convert("RGB")
    assert master.size == (825, 825), "cropped master changed"
    # Original scroll corners, feather tip and shaft: no design reconstruction.
    for x, y in ((249, 365), (335, 276), (1005, 284), (940, 889), (866, 978), (363, 978), (602, 927)):
        dx = (x - (CROP[0] + CROP[2]) / 2) / 825 * 52
        dy = (y - (CROP[1] + CROP[3]) / 2) / 825 * 52
        assert math.hypot(dx, dy) < 33, "art extends outside adaptive safe circle"
    for density, scale in DENSITIES.items():
        directory = RES / f"mipmap-{density}"
        for name, image in images(master, scale).items():
            path = directory / name
            if args.check:
                assert path.read_bytes() == png(image), f"stale icon: {path.name} ({density})"
            else:
                directory.mkdir(exist_ok=True)
                path.write_bytes(png(image))
    print("53 icons: 825px crop, 5 densities, adaptive tips inside 66dp safe circle — OK")


if __name__ == "__main__":
    main()
