# Рисует баннер для README: assets/logo.png (1280x400).
# Стиль как у программы: чёрный фон, лаймовая рамка, бегущая по краю (как на заставке),
# знак P2PJack с пакетом между узлами, название Bahnschrift с разрядкой, подпись Consolas.
# Нужен Pillow и шрифты Windows (bahnschrift.ttf, consola.ttf).
import os
from PIL import Image, ImageDraw, ImageFont

S = 2  # рисуем в 2x и уменьшаем — так края гладкие
W, H = 1280 * S, 400 * S
BG = (6, 6, 6)
LINE2 = (52, 52, 50)
TEXT = (236, 234, 228)
MUTED = (140, 138, 131)
DIM = (85, 84, 80)
ACCENT = (200, 255, 46)
ACCENT_DIM = (93, 117, 26)
WHITE = (255, 255, 255)

FONTS = os.path.join(os.environ.get("WINDIR", r"C:\Windows"), "Fonts")


def font(name, size, variation=None):
    f = ImageFont.truetype(os.path.join(FONTS, name), size * S)
    if variation:
        try:
            f.set_variation_by_name(variation)
        except Exception:
            pass
    return f


def spaced(draw, xy, text, fnt, fill, spacing):
    """Текст с разрядкой; возвращает ширину."""
    x, y = xy
    for ch in text:
        draw.text((x, y), ch, font=fnt, fill=fill)
        x += draw.textlength(ch, font=fnt) + spacing * S
    return x - xy[0] - spacing * S


def spaced_width(draw, text, fnt, spacing):
    return sum(draw.textlength(ch, font=fnt) for ch in text) + spacing * S * (len(text) - 1)


def rect(draw, cx, cy, size, fill):
    h = size * S / 2
    draw.rectangle([cx - h, cy - h, cx + h, cy + h], fill=fill)


img = Image.new("RGB", (W, H), BG)
d = ImageDraw.Draw(img, "RGBA")

# Рамка: тусклая дорожка по краю и лаймовая трасса от середины верха в обе стороны.
m = 22 * S
left, top, right, bottom = m, m, W - m, H - m
d.rectangle([left, top, right, bottom], outline=LINE2, width=S)
stop = top + (bottom - top) * 0.62  # трасса «дошла» до 62% высоты боковых сторон
for glow, color in ((6 * S, (*ACCENT, 40)), (2 * S, ACCENT)):
    d.line([(left, stop), (left, top), (right, top), (right, stop)], fill=color, width=glow, joint="curve")
for x in (left, right):
    rect(d, x, stop, 12, (*ACCENT, 50))
    rect(d, x, stop, 5, WHITE)

# Знак: два узла, линия, пакет со шлейфом; уголки видоискателя вокруг.
cx, cy = 370 * S, H / 2
gap, node = 34 * S, 26
d.line([(cx - gap + node * S / 2, cy), (cx + gap - node * S / 2, cy)], fill=ACCENT_DIM, width=3 * S)
packet_x = cx + 8 * S
for i in range(5, 0, -1):
    alpha = int(180 * (1 - i / 6))
    rect(d, packet_x - i * 3.5 * S, cy, 8 - i * 0.8, (*ACCENT, alpha))
rect(d, packet_x, cy, 8, WHITE)
rect(d, cx + gap, cy, node + 14, (*ACCENT, 55))  # узел, в который прилетает пакет, светится
rect(d, cx - gap, cy, node, ACCENT)
rect(d, cx + gap, cy, node, ACCENT)
b, arm = 78 * S, 18 * S
for sx, sy in ((-1, -1), (1, -1), (1, 1), (-1, 1)):
    x, y = cx + sx * b, cy + sy * b
    d.line([(x, y), (x - sx * arm, y)], fill=MUTED, width=2 * S)
    d.line([(x, y), (x, y - sy * arm)], fill=MUTED, width=2 * S)

# Название и подпись.
title = font("bahnschrift.ttf", 92, "SemiBold")
tx = 500 * S
asc, _ = title.getmetrics()
spaced(d, (tx, cy - asc + 26 * S), "P2PJACK", title, TEXT, 14)
sub = font("consola.ttf", 21)
d.text((tx + 4 * S, cy + 46 * S), "виртуальная локальная сеть для игр", font=sub, fill=MUTED)
d.text((tx + 4 * S, cy + 76 * S), "напрямую · без сервера · на Rust", font=sub, fill=DIM)

# Детали по углам, как на приборной панели.
small = font("consola.ttf", 15)
d.text((left + 22 * S, bottom - 40 * S), "10.77.0.0/16", font=small, fill=DIM)
tag = "P2P · UDP · DHT"
d.text((right - 22 * S - d.textlength(tag, font=small), bottom - 40 * S), tag, font=small, fill=DIM)
rect(d, left + 14 * S, bottom - 31 * S, 5, ACCENT)

out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "assets", "logo.png")
img.resize((W // S, H // S), Image.LANCZOS).save(out, optimize=True)
print("saved", os.path.normpath(out))
