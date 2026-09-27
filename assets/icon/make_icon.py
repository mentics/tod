"""Renders tod's app icon: `tod.ico` (every Windows size) and `tod.png` (1024px).

Run from the repository root: `python assets/icon/make_icon.py`. Needs Pillow.
Each size is drawn on its own, 8x supersampled, so small sizes keep crisp
strokes instead of being a blurred downscale of the large one. At 16 and 24 px
the outline rows would be mush, so those sizes show only the check.
"""

from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
SS = 8  # supersampling factor

TOP = (38, 38, 44)  # charcoal
BOTTOM = (8, 8, 10)  # near black
INK = (255, 255, 255)
DIM = (255, 255, 255, 170)
RIM = (78, 78, 88)  # keeps the tile's edge visible on a dark taskbar
CHECK_BG = (250, 204, 21)  # amber circle behind the check
CHECK_INK = (10, 10, 12)


def gradient(size: int) -> Image.Image:
    col = Image.new("RGBA", (1, size))
    for y in range(size):
        t = y / max(size - 1, 1)
        col.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(TOP, BOTTOM)) + (255,))
    return col.resize((size, size))


def check(draw: ImageDraw.ImageDraw, cx: float, cy: float, r: float, width: float, ink) -> None:
    pts = [(cx - 0.52 * r, cy + 0.02 * r), (cx - 0.14 * r, cy + 0.40 * r), (cx + 0.56 * r, cy - 0.38 * r)]
    draw.line(pts, fill=ink, width=round(width), joint="curve")
    for x, y in (pts[0], pts[-1]):
        h = width / 2
        draw.ellipse((x - h, y - h, x + h, y + h), fill=ink)


def bar(draw: ImageDraw.ImageDraw, x0: float, y: float, x1: float, h: float, fill) -> None:
    draw.rounded_rectangle((x0, y - h / 2, x1, y + h / 2), radius=h / 2, fill=fill)


def render(px: int) -> Image.Image:
    s = px * SS
    u = s / 100  # draw in a 100-unit design grid

    # Rounded-square tile, inset a little so it sits like other app icons.
    inset = 4 * u if px >= 32 else 1 * u
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle((inset, inset, s - inset, s - inset), radius=22 * u, fill=255)
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    img.paste(gradient(s), (0, 0), mask)
    rim = max(s // px, round(1.2 * u))  # at least one output pixel
    ImageDraw.Draw(img).rounded_rectangle(
        (inset, inset, s - inset, s - inset), radius=22 * u, outline=RIM, width=rim
    )

    layer = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)

    if px <= 24:
        # Just the check, as large as the tile allows.
        check(d, 50 * u, 52 * u, 46 * u, 15 * u, INK)
    else:
        # An outline: a done parent row, then two indented child rows.
        r = 13 * u
        cx, cy = 29 * u, 31 * u
        d.ellipse((cx - r, cy - r, cx + r, cy + r), fill=CHECK_BG)
        check(d, cx, cy + 0.5 * u, r * 0.95, 4.2 * u, CHECK_INK)
        bar(d, 48 * u, cy, 80 * u, 9 * u, INK)

        # Tree guide from the parent down to its children, drawn opaque on its
        # own layer and faded as a whole so the joints don't double up.
        g = 3.2 * u
        guides = Image.new("L", (s, s), 0)
        gd = ImageDraw.Draw(guides)
        gd.rounded_rectangle((cx - g / 2, cy + r + 3 * u, cx + g / 2, 72 * u + g / 2), radius=g / 2, fill=DIM[3])
        for y in (55 * u, 72 * u):
            gd.rounded_rectangle((cx - g / 2, y - g / 2, 42 * u, y + g / 2), radius=g / 2, fill=DIM[3])
        faded = Image.new("RGBA", (s, s), DIM[:3] + (255,))
        faded.putalpha(guides)
        layer.alpha_composite(faded)
        for y in (55 * u, 72 * u):
            dot = 4.5 * u
            d.ellipse((47 * u - dot, y - dot, 47 * u + dot, y + dot), fill=INK)
        bar(d, 57 * u, 55 * u, 80 * u, 7.5 * u, INK)
        bar(d, 57 * u, 72 * u, 72 * u, 7.5 * u, INK)

    img = Image.alpha_composite(img, layer)
    return img.resize((px, px), Image.LANCZOS)


def main() -> None:
    images = [render(px) for px in ICO_SIZES]
    largest = images[-1]
    largest.save(
        HERE / "tod.ico",
        format="ICO",
        sizes=[(px, px) for px in ICO_SIZES],
        append_images=images[:-1],
    )
    render(1024).save(HERE / "tod.png")


if __name__ == "__main__":
    main()
