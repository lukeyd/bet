#!/usr/bin/env python3
"""framediff.py — pixel-diff two raw 8-bit DOOM frames (320x200, paletted) against the WAD
PLAYPAL, for the W3 render-parity grind. Stdlib-only (no PIL/numpy): writes PNGs by hand.

Usage:
    python3 framediff.py OURS.raw ORACLE.raw [--wad PATH] [--out DIR] [--pal N]

Produces in --out (default /tmp):
    frame_ours.png    — our frame, PLAYPAL-colored
    frame_oracle.png  — oracle frame, PLAYPAL-colored
    frame_sxs.png     — side-by-side ours | oracle | diff-mask (mismatched pixels in magenta)

Prints: total mismatched pixel count, and a histogram of the most common (ours_idx, oracle_idx)
palette-index pairs at mismatched pixels — a palette/colormap offset shows as a constant delta, a
flat/texture swap as a cluster of index pairs, geometry as scattered pairs.
"""
import struct, sys, zlib, os

W, H = 320, 200
DEFAULT_WAD = "doom-reference/doom1.wad"


def read_playpal(wad_path, palnum=0):
    wad = open(wad_path, "rb").read()
    magic, numl, diroff = struct.unpack("<4sII", wad[:12])
    for i in range(numl):
        off, sz, name = struct.unpack("<II8s", wad[diroff + i * 16: diroff + i * 16 + 16])
        nm = name.rstrip(b"\0").decode("ascii", "replace")
        if nm == "PLAYPAL":
            base = off + palnum * 768
            pal = wad[base: base + 768]
            return [(pal[j * 3], pal[j * 3 + 1], pal[j * 3 + 2]) for j in range(256)]
    raise SystemExit("PLAYPAL not found in WAD")


def write_png(path, rgb, w, h):
    """rgb: bytes/bytearray of length w*h*3 (RGB rows top-to-bottom)."""
    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)
    raw = bytearray()
    stride = w * 3
    for y in range(h):
        raw.append(0)  # filter: none
        raw.extend(rgb[y * stride:(y + 1) * stride])
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    open(path, "wb").write(png)


def colorize(frame, pal):
    out = bytearray(len(frame) * 3)
    for i, idx in enumerate(frame):
        r, g, b = pal[idx]
        out[i * 3] = r
        out[i * 3 + 1] = g
        out[i * 3 + 2] = b
    return out


def main():
    args = [a for a in sys.argv[1:]]
    wad = DEFAULT_WAD
    outdir = "/tmp"
    palnum = 0
    pos = []
    i = 0
    while i < len(args):
        if args[i] == "--wad":
            wad = args[i + 1]; i += 2
        elif args[i] == "--out":
            outdir = args[i + 1]; i += 2
        elif args[i] == "--pal":
            palnum = int(args[i + 1]); i += 2
        else:
            pos.append(args[i]); i += 1
    if len(pos) < 2:
        raise SystemExit(__doc__)
    ours = open(pos[0], "rb").read()
    oracle = open(pos[1], "rb").read()
    if len(ours) != W * H or len(oracle) != W * H:
        raise SystemExit("frames must be %d bytes (got %d, %d)" % (W * H, len(ours), len(oracle)))
    pal = read_playpal(wad, palnum)

    write_png(os.path.join(outdir, "frame_ours.png"), colorize(ours, pal), W, H)
    write_png(os.path.join(outdir, "frame_oracle.png"), colorize(oracle, pal), W, H)

    # side-by-side + diff mask
    gap = 4
    sw = W * 3 + gap * 2
    sxs = bytearray(sw * H * 3)
    co = colorize(ours, pal)
    cor = colorize(oracle, pal)
    mism = 0
    hist = {}
    rowdiff = [0] * H
    coldiff = [0] * W
    for y in range(H):
        for x in range(W):
            p = y * W + x
            a = ours[p]; b = oracle[p]
            # panel 1: ours
            sxs[(y * sw + x) * 3: (y * sw + x) * 3 + 3] = co[p * 3:p * 3 + 3]
            # panel 2: oracle
            x2 = W + gap + x
            sxs[(y * sw + x2) * 3: (y * sw + x2) * 3 + 3] = cor[p * 3:p * 3 + 3]
            # panel 3: diff mask
            x3 = 2 * (W + gap) + x
            if a != b:
                mism += 1
                hist[(a, b)] = hist.get((a, b), 0) + 1
                rowdiff[y] += 1
                coldiff[x] += 1
                sxs[(y * sw + x3) * 3: (y * sw + x3) * 3 + 3] = b"\xff\x00\xff"
            else:
                # show matched pixels dimmed (grayscale of oracle luminance)
                r, g, bb = cor[p * 3], cor[p * 3 + 1], cor[p * 3 + 2]
                lum = (r * 30 + g * 59 + bb * 11) // 100 // 3
                sxs[(y * sw + x3) * 3: (y * sw + x3) * 3 + 3] = bytes((lum, lum, lum))
    write_png(os.path.join(outdir, "frame_sxs.png"), sxs, sw, H)

    print("frames: %s (ours) vs %s (oracle)" % (pos[0], pos[1]))
    print("mismatched pixels: %d / %d  (%.1f%%)" % (mism, W * H, 100.0 * mism / (W * H)))
    print("\ntop (ours_idx -> oracle_idx) pairs at mismatches:")
    print("  %-6s %-6s %-8s %-6s" % ("ours", "oracle", "count", "delta"))
    for (a, b), c in sorted(hist.items(), key=lambda kv: -kv[1])[:30]:
        print("  %-6d %-6d %-8d %+d" % (a, b, c, b - a))

    # where do diffs concentrate?
    def band(counts, n, label):
        step = len(counts) // n
        parts = []
        for k in range(n):
            s = sum(counts[k * step:(k + 1) * step])
            parts.append(s)
        print("%s diff distribution (%d bands): %s" % (label, n, parts))
    print()
    band(rowdiff, 10, "row(y)")
    band(coldiff, 10, "col(x)")

    # split top vs bottom (ceiling/upper vs floor/lower) — nukage/floor lives lower
    top = sum(rowdiff[:100]); bot = sum(rowdiff[100:])
    print("top half (y<100) mismatches: %d ; bottom half (y>=100): %d" % (top, bot))


if __name__ == "__main__":
    main()
