#!/usr/bin/env bash
# Скачивает зафиксированную статическую сборку ffmpeg (BtbN, GPL) для встраивания в приложение
# и кладёт ffmpeg, ffprobe и LICENSE в app/src-tauri/resources/ffmpeg/.
#
#   tools/fetch-ffmpeg.sh linux|windows [папка-назначения]
#
# Выпуски autobuild-* BtbN со временем удаляются. Если ссылка перестала работать — обновите
# TAG/BUILD и контрольные суммы по https://github.com/BtbN/FFmpeg-Builds/releases.
set -euo pipefail

TAG="autobuild-2026-10-21-13-06"
BUILD="ffmpeg-n8.1.3-20-gc8de997cb1"
SHA_LINUX="35168ba5f8edfa264eb2e2e3071d1b9d67d88ae667b665b284cb4be7f68ab23c"
SHA_WINDOWS="3c018ceab05450a5ca7b628d102eee676ef6124580fab0c0bc033570d70523d5"

platform="${1:?укажите linux или windows}"
root="$(cd "$(dirname "$0")/.." && pwd)"
dest="${2:-$root/app/src-tauri/resources/ffmpeg}"

case "$platform" in
  linux) archive="$BUILD-linux64-gpl-8.1.tar.xz"; sha="$SHA_LINUX"; exe="" ;;
  windows) archive="$BUILD-win64-gpl-8.1.zip"; sha="$SHA_WINDOWS"; exe=".exe" ;;
  *) echo "неизвестная платформа: $platform" >&2; exit 2 ;;
esac

if [[ -x "$dest/ffmpeg$exe" && -x "$dest/ffprobe$exe" && -f "$dest/.build" && "$(cat "$dest/.build")" == "$BUILD-$platform" ]]; then
  echo "ffmpeg уже на месте: $dest"
  exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
echo "скачиваю $archive"
curl -fSL --retry 3 -o "$work/$archive" "https://github.com/BtbN/FFmpeg-Builds/releases/download/$TAG/$archive"
echo "$sha  $work/$archive" | sha256sum -c -

mkdir -p "$work/x"
if [[ "$archive" == *.zip ]]; then
  unzip -q "$work/$archive" -d "$work/x"
else
  tar -xJf "$work/$archive" -C "$work/x"
fi
src="$work/x/$(basename "$archive" | sed -E 's/\.(tar\.xz|zip)$//')"

rm -rf "$dest"
mkdir -p "$dest"
cp "$src/bin/ffmpeg$exe" "$src/bin/ffprobe$exe" "$dest/"
cp "$src/LICENSE.txt" "$dest/LICENSE.txt"
echo "$BUILD-$platform" > "$dest/.build"
echo "готово: $dest"
