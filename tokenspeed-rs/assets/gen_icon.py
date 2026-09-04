# 生成 tokenspeed 应用图标 assets/app.ico（纯标准库，无 PIL 依赖）
# 图案与悬浮条/托盘一致：深色圆角方块 + 三根递增的速度柱（绿色）
import struct, os

BG = (26, 26, 26)        # 深色底，与悬浮条一致
GREEN = (74, 222, 128)   # ZCode 工具色
SIZES = [16, 24, 32, 48, 64, 128, 256]


def rounded_inside(x, y, s, r):
    """点是否在圆角方块内"""
    if r <= 0:
        return True
    # 四个角
    for cx, cy in [(r, r), (s - 1 - r, r), (r, s - 1 - r), (s - 1 - r, s - 1 - r)]:
        in_corner_zone = (x < r or x > s - 1 - r) and (y < r or y > s - 1 - r)
        if in_corner_zone:
            if (x - cx) ** 2 + (y - cy) ** 2 > r * r:
                return False
    return True


def draw(size):
    """返回 (xor_bgra_bytes, and_mask_bytes)，底向上行序"""
    px = [[None] * size for _ in range(size)]
    r = max(2, round(size * 0.22))
    bw = max(2, round(size * 0.13))
    gap = max(1, round(size * 0.09))
    total = 3 * bw + 2 * gap
    x0 = (size - total) // 2
    yb = round(size * 0.80)
    heights = [round(size * 0.30), round(size * 0.46), round(size * 0.62)]

    for y in range(size):
        for x in range(size):
            if not rounded_inside(x, y, size, r):
                continue
            c = BG
            for i, h in enumerate(heights):
                bx0 = x0 + i * (bw + gap)
                if bx0 <= x < bx0 + bw and yb - h <= y <= yb:
                    c = GREEN
                    break
            px[y][x] = c

    # XOR (BGRA，底向上)
    xor = bytearray()
    stride = size * 4
    for y in range(size - 1, -1, -1):
        for x in range(size):
            c = px[y][x]
            if c is None:
                xor += b"\x00\x00\x00\x00"
            else:
                xor += bytes([c[2], c[1], c[0], 255])
    # AND 掩码 (1bpp，底向上，行按 32bit 对齐)；32bpp 图标下全 0=不透明即可
    mask_stride = ((size + 31) // 32) * 4
    andm = bytearray(mask_stride * size)

    header = struct.pack(
        "<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, len(xor) + len(andm), 0, 0, 0, 0
    )
    return bytes(header) + bytes(xor) + bytes(andm)


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    images = [draw(s) for s in SIZES]
    out = struct.pack("<HHH", 0, 1, len(SIZES))
    offset = 6 + 16 * len(SIZES)
    entries = b""
    for s, img in zip(SIZES, images):
        sb = 0 if s >= 256 else s
        entries += struct.pack("<BBBBHHII", sb, sb, 0, 0, 1, 32, len(img), offset)
        offset += len(img)
    data = out + entries + b"".join(images)
    path = os.path.join(here, "app.ico")
    with open(path, "wb") as f:
        f.write(data)
    print("written", path, len(data), "bytes")


if __name__ == "__main__":
    main()
