#!/usr/bin/env python3
"""Regenerate md-viewer icons. See assets/icons/README.md.

Needs: fonttools, pillow (pip), and `resvg` on PATH (cargo install resvg).
Font: DejaVu Sans Mono Bold (Bitstream Vera / DejaVu license, embedding outlines is permitted).
"""
import os, subprocess, sys, tempfile
from fontTools.ttLib import TTFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
FONT = os.environ.get("MD_ICON_FONT", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf")
TEXT = "[md]"

def text_path(size, spacing, cx, cy, embolden=0, TEXT=TEXT):
    f = TTFont(FONT)
    gs, cmap, hmtx = f.getGlyphSet(), f.getBestCmap(), f["hmtx"]
    s = size / f["head"].unitsPerEm
    # layout in font units first to measure the bbox
    from fontTools.pens.boundsPen import BoundsPen
    x = 0; items = []
    xmin = ymin = 1e9; xmax = ymax = -1e9
    for ch in TEXT:
        n = cmap[ord(ch)]
        bp = BoundsPen(gs); gs[n].draw(bp)
        b = bp.bounds
        xmin = min(xmin, x + b[0]); xmax = max(xmax, x + b[2])
        ymin = min(ymin, b[1]); ymax = max(ymax, b[3])
        items.append((n, x)); x += hmtx[n][0] + spacing / s
    ox = cx - (xmin + xmax) / 2 * s
    oy = cy + (ymin + ymax) / 2 * s   # y flipped
    d = []
    for n, x in items:
        pen = SVGPathPen(gs, ntos=lambda v: ("%.2f" % v).rstrip("0").rstrip("."))
        gs[n].draw(TransformPen(pen, (s, 0, 0, -s, ox + x * s, oy)))
        d.append(pen.getCommands())
    return " ".join(d), embolden

def full_svg():
    d, _ = text_path(160, -5, 256, 262)
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">
  <title>md-viewer</title>
  <defs>
    <linearGradient id="bg" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#334155"/>
      <stop offset="100%" stop-color="#0f172a"/>
    </linearGradient>
    <filter id="shadow" x="-10%" y="-10%" width="120%" height="120%">
      <feDropShadow dx="0" dy="12" stdDeviation="15" flood-opacity="0.4"/>
    </filter>
  </defs>
  <rect x="48" y="48" width="416" height="416" rx="96" fill="url(#bg)" filter="url(#shadow)"/>
  <rect x="52" y="52" width="408" height="408" rx="92" fill="none" stroke="#475569" stroke-width="6"/>
  <!-- "[md]" outlined from DejaVu Sans Mono Bold (no font dependency) -->
  <path fill="#38bdf8" d="{d}"/>
</svg>
'''

def small_svg():
    # Simplified for 16/24 px: no border/shadow, brackets dropped, larger bolder "md".
    d, _ = text_path(340, -10, 256, 262, TEXT="md")
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">
  <rect width="512" height="512" rx="104" fill="#1e293b"/>
  <path fill="#38bdf8" stroke="#38bdf8" stroke-width="16" stroke-linejoin="round" d="{d}"/>
</svg>
'''

def render(svg, px, out):
    with tempfile.NamedTemporaryFile("w", suffix=".svg", delete=False) as t:
        t.write(svg)
    subprocess.check_call(["resvg", "-w", str(px), "-h", str(px), t.name, out])
    os.unlink(t.name)

def main():
    full, small = full_svg(), small_svg()
    open(os.path.join(HERE, "md-viewer.svg"), "w").write(full)
    sizes = [16, 24, 32, 48, 64, 128, 256, 512]
    for px in sizes:
        render(small if px <= 24 else full, px, os.path.join(HERE, f"md-viewer-{px}.png"))
    imgs = [Image.open(os.path.join(HERE, f"md-viewer-{p}.png")).convert("RGBA") for p in (16, 24, 32, 48, 64, 128, 256)]
    imgs[-1].save(os.path.join(HERE, "md-viewer.ico"), format="ICO",
                  sizes=[(i.width, i.height) for i in imgs], append_images=imgs[:-1])
    # raw RGBA for the runtime window icon (64x64, include_bytes! in md-viewer)
    open(os.path.join(HERE, "md-viewer-64.rgba"), "wb").write(
        Image.open(os.path.join(HERE, "md-viewer-64.png")).convert("RGBA").tobytes())

if __name__ == "__main__":
    main()
