"""Draw RelayPlayer's two stacked screens and play symbol from simple geometry.

Run `python tools/make_icon.py` with Pillow 12.2.0 to reproduce the checked-in
Windows icon, 256 px PNG and the 64 px RGBA buffer used by the Rust host.
The build uses those assets directly; Python and Pillow are optional.
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageOps

ASSETS = Path(__file__).resolve().parents[1] / "assets"
SCALE = 16  # drawing coordinates are in a 64 x 64 design grid
SIDE = 64 * SCALE
SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)


def gradient(mask, top, bottom):
    """Paint diagonally from the upper left to the lower right inside mask."""
    box = mask.getbbox()
    w, h = box[2] - box[0], box[3] - box[1]
    ramp = Image.linear_gradient("L")
    dx = ramp.transpose(Image.Transpose.ROTATE_90).resize((w, h))
    dy = ramp.resize((w, h))
    fill = ImageOps.colorize(Image.blend(dx, dy, 0.58), top, bottom)
    tile = Image.new("RGBA", (SIDE, SIDE))
    tile.paste(fill, box, mask.crop(box))
    return tile


def card(box, radius, top, bottom):
    mask = Image.new("L", (SIDE, SIDE))
    ImageDraw.Draw(mask).rounded_rectangle(
        [round(n * SCALE) for n in box], round(radius * SCALE), fill=255
    )
    return gradient(mask, top, bottom)


def play_symbol():
    # Three quadratic corners keep the silhouette legible at taskbar sizes.
    points = [(28.0, 19.5)]
    def corner(control, end):
        start = points[-1]
        for step in range(1, 13):
            t = step / 12
            points.append(tuple((1 - t)**2 * start[i] + 2 * (1 - t) * t * control[i]
                                + t**2 * end[i] for i in (0, 1)))

    corner((28.0, 16.5), (30.6, 18.0))
    points.append((45.4, 26.6))
    corner((48.0, 28.1), (45.4, 29.6))
    points.append((30.6, 38.2))
    corner((28.0, 39.7), (28.0, 36.7))
    mask = Image.new("L", (SIDE, SIDE))
    ImageDraw.Draw(mask).polygon([(round(x*SCALE), round(y*SCALE)) for x, y in points], fill=255)
    return gradient(mask, "#ffffff", "#e3e6ff")


def main():
    # The offset indigo screen and the raised blue-violet screen have a thin
    # brighter rim. No fonts, logos, images or other external art are used.
    master = card((4, 17, 53, 55), 9, "#9d9cff", "#4e3db4")
    master.alpha_composite(card((5, 18, 52, 54), 8, "#7d81ee", "#4337a2"))
    master.alpha_composite(card((11, 9, 60, 47), 9, "#b1caff", "#8671ed"))
    master.alpha_composite(card((12, 10, 59, 46), 8, "#548dff", "#7545d9"))
    master.alpha_composite(play_symbol())
    sizes = {n: master.resize((n, n), Image.Resampling.LANCZOS) for n in SIZES}
    ASSETS.mkdir(exist_ok=True)
    sizes[256].save(ASSETS / "relay.png", compress_level=9)
    sizes[256].save(ASSETS / "relay.ico", sizes=[(n, n) for n in SIZES],
                    append_images=[sizes[n] for n in SIZES if n != 256])
    (ASSETS / "icon.rgba").write_bytes(sizes[64].tobytes())


if __name__ == "__main__":
    main()
