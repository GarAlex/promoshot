#!/usr/bin/env python3
"""The three scrolling backgrounds — stripes, dots, hexagons.

Each feature is hundreds of pixels across on purpose: the pan has to read
on the canvas, through the laptop's screen, and through the tablet's
screen inside it, and a finer texture is flat colour at that depth.
Writes into resources/ (or the folder given as the first argument)."""
import math, os, sys
from PIL import Image, ImageDraw

R = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.abspath(__file__)), 'resources')
W, H = 4096, 2700

def stripes(base, tone, period=520, width=240, angle=-28):
    big = Image.new('RGB', (W * 2, H * 2), base); d = ImageDraw.Draw(big)
    lean = 2 * H * math.tan(math.radians(angle))
    for x in range(-W * 2, W * 3, period):
        d.polygon([(x, 0), (x + width, 0), (x + width - lean, 2 * H), (x - lean, 2 * H)], fill=tone)
    return big.resize((W, H), Image.LANCZOS)

def dots(base, tone, step=420, r=150):
    im = Image.new('RGB', (W, H), base); d = ImageDraw.Draw(im)
    for j, y in enumerate(range(-step, H + step, step)):
        for x in range(-step + (step // 2 if j % 2 else 0), W + step, step):
            d.ellipse([x - r, y - r, x + r, y + r], fill=tone)
    return im

def hexes(base, tone, r=230):
    im = Image.new('RGB', (W, H), base); d = ImageDraw.Draw(im)
    dx = r * math.sqrt(3); dy = r * 1.5
    for j, y in enumerate(range(-int(dy), H + int(dy), int(dy))):
        for x in range(-int(dx) + (int(dx / 2) if j % 2 else 0), W + int(dx), int(dx)):
            d.polygon([(x + r * 0.86 * math.cos(math.radians(60 * k + 30)),
                        y + r * 0.86 * math.sin(math.radians(60 * k + 30))) for k in range(6)], fill=tone)
    return im

def vignette(im):
    """Darker towards the bottom, where the floor meets it."""
    grad = Image.linear_gradient('L').resize((W, H)).point(lambda v: int(v * 0.28))
    return Image.composite(Image.new('RGB', (W, H), (0, 0, 0)), im, grad)

if __name__ == '__main__':
    os.makedirs(R, exist_ok=True)
    vignette(stripes((62, 86, 150), (96, 128, 205))).save(os.path.join(R, 'bg_laptop.png'), optimize=True)
    vignette(dots((118, 72, 46), (196, 132, 84))).save(os.path.join(R, 'bg_tablet.png'), optimize=True)
    vignette(hexes((36, 96, 78), (70, 158, 124))).save(os.path.join(R, 'bg_phone.png'), optimize=True)
    print({f: os.path.getsize(os.path.join(R, f)) // 1024 for f in ('bg_laptop.png', 'bg_tablet.png', 'bg_phone.png')})
