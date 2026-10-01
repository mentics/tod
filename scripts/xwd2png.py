#!/usr/bin/env python3
"""Convert the framebuffer file Xvfb writes (`-fbdir DIR` -> DIR/Xvfb_screen0,
an XWD) to a PNG, with no dependencies. 32 bits per pixel, as Xvfb's 24-bit
screen is. Usage: xwd2png.py Xvfb_screen0 out.png"""
import struct, sys, zlib

data = open(sys.argv[1], 'rb').read()
f = struct.unpack('>25I', data[:100])
header, w, h, bpp, bpl, ncolors = f[0], f[4], f[5], f[11], f[12], f[19]
assert bpp == 32, f'expected 32 bits per pixel, got {bpp}'
base = header + ncolors * 12
rows = []
for y in range(h):
    row = data[base + y * bpl: base + y * bpl + w * 4]
    rgb = bytearray(w * 3)
    rgb[0::3], rgb[1::3], rgb[2::3] = row[2::4], row[1::4], row[0::4]
    rows.append(b'\x00' + bytes(rgb))

def chunk(kind, body):
    return struct.pack('>I', len(body)) + kind + body + struct.pack('>I', zlib.crc32(kind + body) & 0xffffffff)

png = (b'\x89PNG\r\n\x1a\n'
       + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0))
       + chunk(b'IDAT', zlib.compress(b''.join(rows), 6))
       + chunk(b'IEND', b''))
open(sys.argv[2], 'wb').write(png)
