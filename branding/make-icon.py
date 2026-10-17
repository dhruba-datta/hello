"""Draws Hello's mark and builds every icon from it.

The mark is a row of rounded bars of uneven height, like a voice. It appears white on the
sunset gradient in the app icon, and alone (as a template image) in the menu bar.

Usage: python3 branding/make-icon.py [website/assets]
Writes branding/AppIcon-1024.png, branding/AppIcon.icns and branding/StatusIcon.png. With a
directory, it also writes the website's icon-1024, icon-256, favicon-64 and apple-touch-icon.
Needs Pillow; AppIcon.icns needs macOS iconutil.
"""
import os
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw, ImageFilter

here = os.path.dirname(os.path.abspath(__file__))

# Bar heights as a share of the tallest bar.
HEIGHTS = [0.30, 0.66, 0.44, 1.0, 0.58, 0.84, 0.36]
# Bar width as a share of the distance between bars.
BAR_WIDTH = 0.62
SUNSET = [(0.0, (255, 146, 64)), (0.5, (255, 77, 122)), (1.0, (196, 50, 160))]
WHITE = (255, 255, 255, 255)
SUPERSAMPLE = 4

# macOS icon grid on a 1024 canvas.
CANVAS, BODY, CORNER = 1024, 824, 185
MARK_SHARE = 0.6


def mark(width, height, color):
    """The mark filling a transparent `width` x `height` canvas."""
    w, h = width * SUPERSAMPLE, height * SUPERSAMPLE
    image = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    pitch = w / len(HEIGHTS)
    bar = pitch * BAR_WIDTH
    for index, share in enumerate(HEIGHTS):
        center = pitch * (index + 0.5)
        length = max(share * h, bar)
        top = (h - length) / 2
        draw.rounded_rectangle(
            [center - bar / 2, top, center + bar / 2, top + length], radius=bar / 2, fill=color
        )
    return image.resize((width, height), Image.LANCZOS)


def gradient(size, stops):
    """A diagonal gradient from the top-left corner to the bottom-right one."""
    image = Image.new("RGB", (size, size))
    pixels = image.load()
    for y in range(size):
        for x in range(size):
            t = (x + y) / (2 * (size - 1))
            for (t0, c0), (t1, c1) in zip(stops, stops[1:]):
                if t0 <= t <= t1:
                    f = (t - t0) / (t1 - t0)
                    pixels[x, y] = tuple(round(a + (b - a) * f) for a, b in zip(c0, c1))
                    break
    return image


def rounded_mask(size, inset, corner):
    s = size * SUPERSAMPLE
    mask = Image.new("L", (s, s), 0)
    lo, hi = inset * SUPERSAMPLE, (size - inset) * SUPERSAMPLE
    ImageDraw.Draw(mask).rounded_rectangle([lo, lo, hi, hi], radius=corner * SUPERSAMPLE, fill=255)
    return mask.resize((size, size), Image.LANCZOS)


def with_mark(background):
    """`background` with the white mark centered on the icon body."""
    image = background.convert("RGBA")
    side = round(BODY * MARK_SHARE * image.width / CANVAS)
    glyph = mark(side, side, WHITE)
    image.alpha_composite(glyph, ((image.width - side) // 2, (image.height - side) // 2))
    return image


def app_icon():
    """The full 1024 icon: rounded body with a soft shadow, as macOS draws app icons."""
    mask = rounded_mask(CANVAS, (CANVAS - BODY) // 2, CORNER)
    icon = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    shadow = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    shadow.putalpha(mask.point(lambda a: round(a * 0.35)))
    icon.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(14)), (0, 10))
    body = with_mark(gradient(CANVAS, SUNSET))
    body.putalpha(mask)
    icon.alpha_composite(body)
    return icon


def write_icns(master):
    with tempfile.TemporaryDirectory() as work:
        iconset = os.path.join(work, "AppIcon.iconset")
        os.mkdir(iconset)
        for points in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                pixels = points * scale
                suffix = "@2x" if scale == 2 else ""
                master.resize((pixels, pixels), Image.LANCZOS).save(
                    os.path.join(iconset, f"icon_{points}x{points}{suffix}.png")
                )
        subprocess.run(
            ["iconutil", "-c", "icns", iconset, "-o", os.path.join(here, "AppIcon.icns")],
            check=True,
        )


def write_website(directory, master):
    master.save(os.path.join(directory, "icon-1024.png"))
    master.resize((256, 256), Image.LANCZOS).save(os.path.join(directory, "icon-256.png"))
    # Small sizes drop the shadow margin so the mark stays readable in a browser tab.
    inset = (CANVAS - BODY) // 2
    body = master.crop((inset, inset, CANVAS - inset, CANVAS - inset))
    body.resize((64, 64), Image.LANCZOS).save(os.path.join(directory, "favicon-64.png"))
    # iOS rounds the corners itself and fills transparency with black, so this one is square.
    touch = with_mark(gradient(CANVAS, SUNSET)).convert("RGB")
    touch.resize((180, 180), Image.LANCZOS).save(os.path.join(directory, "apple-touch-icon.png"))


def main():
    master = app_icon()
    master.save(os.path.join(here, "AppIcon-1024.png"))
    write_icns(master)
    # Menu bar template image at 2x: 19 x 16 points, black on clear (macOS tints it).
    mark(38, 32, (0, 0, 0, 255)).save(os.path.join(here, "StatusIcon.png"))
    if len(sys.argv) > 1:
        write_website(sys.argv[1], master)
    print("wrote AppIcon-1024.png, AppIcon.icns and StatusIcon.png")


if __name__ == "__main__":
    main()
