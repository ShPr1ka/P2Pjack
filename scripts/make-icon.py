# Рисует знак P2PJack (как mark_icon в src/ui_kit.rs) и собирает assets/p2pjack.ico из PNG разных размеров.
import struct, zlib

ACCENT = (0xc8, 0xff, 0x2e)
BG = (0x0a, 0x0a, 0x0a)

def inside(x, y):
    ax, ay = abs(x - 0.5), abs(y - 0.5)
    r = 0.16
    if ax > 0.5 or ay > 0.5:
        return None
    cx, cy = max(ax - (0.5 - r), 0.0), max(ay - (0.5 - r), 0.0)
    if cx * cx + cy * cy > r * r:
        return None
    node = lambda nx: abs(x - nx) <= 0.12 and abs(y - 0.5) <= 0.12
    link = 0.3 <= x <= 0.7 and abs(y - 0.5) <= 0.035
    return ACCENT if node(0.27) or node(0.73) or link else BG

def render(n, ss=8):
    rows = []
    for py in range(n):
        row = bytearray([0])  # PNG filter: none
        for px in range(n):
            acc = [0, 0, 0]; cover = 0
            for sy in range(ss):
                for sx in range(ss):
                    c = inside((px + (sx + 0.5) / ss) / n, (py + (sy + 0.5) / ss) / n)
                    if c:
                        acc[0] += c[0]; acc[1] += c[1]; acc[2] += c[2]; cover += 1
            if cover:
                row += bytes([acc[0] // cover, acc[1] // cover, acc[2] // cover, round(cover / (ss * ss) * 255)])
            else:
                row += bytes(4)
        rows.append(bytes(row))
    return b''.join(rows)

def png(n):
    raw = render(n, 8 if n <= 64 else 3)
    chunk = lambda t, d: struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', n, n, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(raw, 9)) + chunk(b'IEND', b''))

sizes = [16, 20, 24, 32, 40, 48, 64, 256]
images = [png(n) for n in sizes]
header = struct.pack('<HHH', 0, 1, len(sizes))
offset = 6 + 16 * len(sizes)
entries = b''
for n, data in zip(sizes, images):
    entries += struct.pack('<BBBBHHII', n % 256, n % 256, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
open('assets/p2pjack.ico', 'wb').write(header + entries + b''.join(images))
open('assets/p2pjack-256.png', 'wb').write(images[-1])
print('ok', offset)
