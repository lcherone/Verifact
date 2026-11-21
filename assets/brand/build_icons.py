#!/usr/bin/env python3
"""Rasterize Verifact's original SVG and package macOS icon sizes.

Requires CairoSVG 2.9.1. On Homebrew macOS, make libcairo discoverable with
DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib when invoking this script.
"""
from pathlib import Path
import subprocess
import sys
import cairosvg

brand = Path(__file__).resolve().parent
source = brand / 'verifact-app-icon.svg'
cairosvg.svg2png(url=str(source), write_to=str(brand / 'verifact-app-icon.png'), output_width=1024, output_height=1024)
if sys.platform == 'darwin':
    iconset = brand.parents[1] / '.tools' / 'verifact.iconset'
    iconset.mkdir(parents=True, exist_ok=True)
    for base in (16, 32, 128, 256, 512):
        for scale in (1, 2):
            suffix = '@2x' if scale == 2 else ''
            destination = iconset / f'icon_{base}x{base}{suffix}.png'
            cairosvg.svg2png(url=str(source), write_to=str(destination), output_width=base * scale, output_height=base * scale)
    subprocess.run(['/usr/bin/iconutil', '-c', 'icns', str(iconset), '-o', str(brand / 'Verifact.icns')], check=True)
