#!/usr/bin/env bash
# Дымовой тест готовой консольной программы: HEIC → JPEG (образец из crates/core/tests/fixtures)
# и видео HEVC → MP4 (ролик делает сам ffmpeg). В выпуске запускается на собранных файлах
# для Linux и Windows — ловит то, чего не видят модульные тесты: нет DLL рядом с программой,
# не находится ffmpeg, не открывается HEIC.
#
#   tools/smoke-test.sh <путь к converter[.exe]>     # ffmpeg и ffprobe — в PATH
set -euo pipefail

cli="${1:?укажите путь к converter}"
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# В Git Bash на Windows программы ждут пути вида C:/…, а не /tmp/…
command -v cygpath >/dev/null && work="$(cygpath -m "$work")"
mkdir "$work/in" "$work/out"

cp "$root/crates/core/tests/fixtures/sample.heic" "$work/in/"
ffmpeg -v error -f lavfi -i testsrc2=s=320x240:rate=30 -f lavfi -i sine=f=440 -t 2 \
  -c:v libx265 -x265-params log-level=error -tag:v hvc1 -c:a aac -shortest "$work/in/clip.mov"

"$cli" hw
"$cli" convert "$work/in" --out "$work/out" --to jpeg --hw off

probe() { ffprobe -v error -select_streams v:0 -show_entries "$1" -of csv=p=0 "$2" | tr -d '\r'; }
fail() { echo "ОШИБКА: $*" >&2; exit 1; }

[[ -f "$work/out/sample.jpg" ]] || fail "нет sample.jpg"
[[ "$(probe stream=codec_name,width,height "$work/out/sample.jpg")" == "mjpeg,640,480" ]] || fail "sample.jpg не JPEG 640×480"
[[ -f "$work/out/clip.mp4" ]] || fail "нет clip.mp4"
[[ "$(probe stream=codec_name "$work/out/clip.mp4")" == "h264" ]] || fail "clip.mp4 не H.264"
duration="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$work/out/clip.mp4" | tr -d '\r')"
awk -v d="$duration" 'BEGIN { exit !(d > 1.9 && d < 2.1) }' || fail "длительность clip.mp4 $duration вместо 2 с"

echo "дымовой тест пройден"
