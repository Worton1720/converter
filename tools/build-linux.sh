#!/usr/bin/env bash
# Сборка пакетов для Linux: .deb (системный ffmpeg по зависимости) и AppImage (со встроенным ffmpeg).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
"$root/tools/fetch-ffmpeg.sh" linux
cd "$root/app"
pnpm install --frozen-lockfile
# libheif вшивается в программу (со встроенным libde265): пакеты не зависят от версии
# и плагинов libheif в системе. Нужны cmake и libde265-dev.
pnpm tauri build --bundles deb --features embedded-libheif
pnpm tauri build --bundles appimage --features embedded-libheif --config src-tauri/tauri.appimage.conf.json
ls -la "$root/target/release/bundle/deb" "$root/target/release/bundle/appimage"
