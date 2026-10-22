#!/usr/bin/env bash
# Собирает значки приложения из SVG: icon.ico (Windows, 16–256 px внутри) и icon.png 512 px
# (Linux: значок окна, .deb и AppImage — Tauri берёт там только PNG).
# 16 и 24 px рисуются из упрощённого icon-small.svg, иначе мелкие детали сливаются.
#
#   tools/icon/make-icon.sh        # нужны rsvg-convert и python3
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
out="$here/../../app/src-tauri/icons"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

for s in 16 24; do rsvg-convert -w "$s" -h "$s" "$here/icon-small.svg" -o "$work/$s.png"; done
for s in 32 48 64 128 256 512; do rsvg-convert -w "$s" -h "$s" "$here/icon.svg" -o "$work/$s.png"; done

mkdir -p "$out"
# ICO из PNG-кадров, как делает `tauri icon` (Windows Vista и новее): втрое меньше, чем BMP.
python3 - "$out/icon.ico" "$work"/{16,24,32,48,64,128,256}.png <<'PY'
import struct, sys
frames = [open(p, "rb").read() for p in sys.argv[2:]]
out = struct.pack("<HHH", 0, 1, len(frames))
offset = 6 + 16 * len(frames)
for png in frames:
    w, h = struct.unpack(">II", png[16:24])
    out += struct.pack("<BBBBHHII", w % 256, h % 256, 0, 0, 1, 32, len(png), offset)
    offset += len(png)
open(sys.argv[1], "wb").write(out + b"".join(frames))
PY
cp "$work/512.png" "$out/icon.png"
echo "готово: $out/icon.ico, $out/icon.png"
