"""Builds branding/AppIcon.icns from the chosen icon option.

Usage: python3 branding/make-icon.py [option-name]   (default: sunset-wave)
Options are defined in icon-options.py.
"""
import importlib.util
import os
import subprocess
import sys
import tempfile

from PIL import Image

here = os.path.dirname(os.path.abspath(__file__))
CHOSEN = "sunset-wave"


def load_options():
    spec = importlib.util.spec_from_file_location("icon_options", os.path.join(here, "icon-options.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    name = sys.argv[1] if len(sys.argv) > 1 else CHOSEN
    options = load_options()
    build = dict(options.OPTIONS)[name]
    master = options.finish(build())
    master.save(os.path.join(here, "AppIcon-1024.png"))
    with tempfile.TemporaryDirectory() as work:
        iconset = os.path.join(work, "AppIcon.iconset")
        os.mkdir(iconset)
        for points in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                pixels = points * scale
                suffix = "@2x" if scale == 2 else ""
                master.resize((pixels, pixels), Image.LANCZOS).save(
                    os.path.join(iconset, f"icon_{points}x{points}{suffix}.png"))
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", os.path.join(here, "AppIcon.icns")],
                       check=True)
    print(f"wrote AppIcon.icns from {name}")


if __name__ == "__main__":
    main()
