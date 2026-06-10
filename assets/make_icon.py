# Regenerate: python3 assets/make_icon.py assets/icon.png (needs Pillow + numpy), then
# resize to assets/icon-512.png (the window / Dock icon embedded in the binary).
# Glasir app icon: one big frosted-glass leaf whose midrib and veins are git
# lanes (rounded joins), golden commit dots where the veins end.
import sys, math, os
import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageChops

S = 4096; k = S / 1024
out = sys.argv[1] if len(sys.argv) > 1 else os.path.splitext(__file__)[0] + '.png'
yy, xx = np.mgrid[0:S, 0:S].astype(np.float32) / S

def sq_mask(inset, n=5.0):
    c = S / 2; r = S / 2 - inset * k
    return ((np.abs(xx * S - c) / r) ** n + (np.abs(yy * S - c) / r) ** n) <= 1.0
def L(a): return Image.fromarray((np.clip(a, 0, 1) * 255).astype(np.uint8))
def solid(col, alpha_img):
    im = Image.new('RGBA', (S, S), col); im.putalpha(alpha_img); return im

# --- background: night gradient + colour blobs ---
t = np.clip(0.65 * yy + 0.35 * xx, 0, 1)[..., None]
bg = np.array([16, 22, 58], np.float32) * (1 - t) + np.array([46, 24, 92], np.float32) * t
for cx, cy, rad, col, a in [(0.25, 0.22, 0.55, (40, 140, 255), 0.55), (0.82, 0.82, 0.60, (190, 60, 220), 0.50), (0.78, 0.20, 0.38, (60, 220, 200), 0.30)]:
    w = (np.clip(1 - np.sqrt((xx - cx) ** 2 + (yy - cy) ** 2) / rad, 0, 1) ** 2 * a)[..., None]
    bg = bg * (1 - w) + np.array(col, np.float32) * w
tile = sq_mask(100)
img = np.dstack([bg, tile * 255]).astype(np.uint8)
comp = Image.fromarray(img, 'RGBA')

# --- leaf geometry (local u along axis from base, v across), tilted ---
ANG = math.radians(32)            # tip leans to the upper right
BX, BY = 318, 790                 # leaf base (stem joins here)
LEN, WID = 700, 440
def to_xy(u, v):
    return (BX + u * math.sin(ANG) + v * math.cos(ANG), BY - u * math.cos(ANG) + v * math.sin(ANG))
def half(t):                      # half width at fraction t of the length
    return WID / 2 * math.sin(math.pi * t) ** 0.75 * (1 - 0.18 * t)
def P(u, v): x, y = to_xy(u, v); return (x * k, y * k)
outline = [P(i / 200 * LEN, half(i / 200)) for i in range(201)] + [P(i / 200 * LEN, -half(i / 200)) for i in range(200, -1, -1)]

leaf_m = Image.new('L', (S, S), 0); ImageDraw.Draw(leaf_m).polygon(outline, fill=255)

# soft shadow under the leaf, a golden halo behind it
sh = leaf_m.filter(ImageFilter.GaussianBlur(26 * k))
sh = sh.transform((S, S), Image.AFFINE, (1, 0, -10 * k, 0, 1, -22 * k)).point(lambda v: v * 0.55)
comp = Image.alpha_composite(comp, solid((6, 6, 24, 255), ImageChops.multiply(sh, L(tile))))
halo = leaf_m.filter(ImageFilter.GaussianBlur(80 * k)).point(lambda v: v * 0.22)
comp = Image.alpha_composite(comp, solid((150, 200, 255, 255), ImageChops.multiply(halo, L(tile))))

# frosted glass body: heavily blurred + lightened background, white wash brighter top-left
frost = comp.filter(ImageFilter.GaussianBlur(40 * k))
fa = np.asarray(frost).astype(np.float32)
wash = (0.30 + 0.30 * np.clip(1.3 - (xx + yy) * 1.0, 0, 1))[..., None]
fa[..., :3] = fa[..., :3] * (1 - wash) + np.array([228, 236, 255], np.float32) * wash
fa[..., 3] = 255
frost = Image.fromarray(fa.astype(np.uint8), 'RGBA')
comp = Image.composite(frost, comp, leaf_m)

# refractive edge: bright rim on the lit side, darker on the far side
edge = Image.new('L', (S, S), 0); ImageDraw.Draw(edge).line(outline + [outline[0]], fill=255, width=int(26 * k), joint='curve')
inner = ImageChops.multiply(edge, leaf_m).filter(ImageFilter.GaussianBlur(6 * k))
light = np.clip(1.25 - (xx + yy) * 1.0, 0, 1)
comp = Image.alpha_composite(comp, solid((255, 255, 255, 255), ImageChops.multiply(inner, L(0.35 + 0.65 * light))))
comp = Image.alpha_composite(comp, solid((40, 20, 90, 255), ImageChops.multiply(inner, L(0.45 * (1 - light)))))
crisp = Image.new('L', (S, S), 0); ImageDraw.Draw(crisp).line(outline + [outline[0]], fill=255, width=int(5 * k), joint='curve')
comp = Image.alpha_composite(comp, solid((255, 255, 255, 255), ImageChops.multiply(crisp, L(0.45 + 0.55 * light))))

# specular streak just inside the lit (upper-left) edge
spec = Image.new('L', (S, S), 0)
ImageDraw.Draw(spec).line([P(i / 100 * LEN, -(half(i / 100) - 20)) for i in range(14, 70)], fill=255, width=int(9 * k), joint='curve')
spec = spec.filter(ImageFilter.GaussianBlur(5 * k))
comp = Image.alpha_composite(comp, solid((255, 255, 255, 255), spec.point(lambda v: v * 0.75)))

# --- the graph inside: midrib lane + vein lanes ---
lay = Image.new('RGBA', (S, S), (0, 0, 0, 0)); d = ImageDraw.Draw(lay)
def stroke(pts, col, w):
    for (x0, y0), (x1, y1) in zip(pts, pts[1:]):
        n = max(1, int(math.hypot(x1 - x0, y1 - y0) / (w * 0.12)))
        for i in range(n + 1):
            x = x0 + (x1 - x0) * i / n; y = y0 + (y1 - y0) * i / n
            d.ellipse((x - w / 2, y - w / 2, x + w / 2, y + w / 2), fill=col)
lane = (58, 48, 140, 255)          # deep indigo lanes read on frosted glass
LW = 30 * k
def vein(u0, side, du, vend, r=120):
    """leave the midrib at u0 with a rounded quarter-ish elbow, run diagonally out"""
    pts = []
    ang_end = math.radians(48)     # final heading relative to the axis
    for i in range(0, 41):
        a = ang_end * i / 40
        pts.append((u0 + r * math.sin(a), side * r * (1 - math.cos(a))))
    ue, ve = pts[-1]
    n = (vend - abs(ve)) / math.sin(ang_end)
    end = (ue + n * math.cos(ang_end), side * (abs(ve) + n * math.sin(ang_end)))
    pts.append(end)
    return [P(u, v) for u, v in pts], end
mid = [P(-50, 0), P(LEN * 0.80, 0)]
veins = [vein(60, -1, 0, 128), vein(180, 1, 0, 132), vein(300, -1, 0, 112)]
for pts, _ in veins: stroke(pts, lane, LW)
stroke(mid, lane, LW)
# commit dots: gold where veins (and the midrib) end, glass rings on the midrib
gold, gold_hi, gold_lo = (246, 190, 60, 255), (255, 228, 140, 255), (214, 140, 30, 255)
def gdot(u, v, r):
    x, y = to_xy(u, v); x *= k; y *= k; r *= k; R = r + 9 * k
    d.ellipse((x - R, y - R, x + R, y + R), fill=(255, 248, 225, 255))
    d.ellipse((x - r, y - r, x + r, y + r), fill=gold_lo)
    d.ellipse((x - r * 0.92, y - r * 0.98, x + r * 0.80, y + r * 0.74), fill=gold)
    d.ellipse((x - r * 0.62, y - r * 0.66, x + r * 0.10, y + r * 0.02), fill=gold_hi)
for _, (u, v) in veins: gdot(u, v, 34)
gdot(LEN * 0.80, 0, 40)
comp = Image.alpha_composite(comp, lay)

# --- liquid-glass tile: top sheen + bright rim, then shadow ---
sheen = np.zeros((S, S, 4), np.float32); sheen[..., :3] = 255
sheen[..., 3] = np.clip(1 - yy / 0.5, 0, 1) ** 1.6 * 0.20 * tile * 255
comp = Image.alpha_composite(comp, Image.fromarray(sheen.astype(np.uint8), 'RGBA'))
rim = sq_mask(100) & ~sq_mask(106)
r = np.zeros((S, S, 4), np.uint8); r[..., :3] = 255; r[..., 3] = (rim * (90 + 120 * (1 - yy))).astype(np.uint8)
comp = Image.alpha_composite(comp, Image.fromarray(r, 'RGBA'))
comp.putalpha(ImageChops.multiply(comp.getchannel('A'), L(tile)))
shm = L(tile * 0.42).filter(ImageFilter.GaussianBlur(28 * k)).transform((S, S), Image.AFFINE, (1, 0, 0, 0, 1, -18 * k))
final = Image.alpha_composite(solid((0, 0, 0, 255), shm), comp)
final.resize((1024, 1024), Image.LANCZOS).save(out)
print('wrote', out)
