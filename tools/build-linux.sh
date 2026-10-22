#!/usr/bin/env bash
# Сборка пакетов для Linux: .deb (системный ffmpeg по зависимости) и AppImage (со встроенным ffmpeg).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
"$root/tools/fetch-ffmpeg.sh" linux
cd "$root/app"
pnpm install --frozen-lockfile
# Файлы автообновления (*.sig) подписываются ключом проекта: в выпуске он приходит из секретов
# GitHub, локально — `TAURI_SIGNING_PRIVATE_KEY=~/.tauri/converter-updater.key` (+ пароль).
# Без ключа пакеты собираются без подписи обновлений.
unsigned=()
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  echo "ключа подписи обновлений нет — собираю без файлов автообновления"
  unsigned=(--config '{"bundle":{"createUpdaterArtifacts":false}}')
fi
# libheif вшивается в программу (со встроенным libde265): пакеты не зависят от версии
# и плагинов libheif в системе. Нужны cmake и libde265-dev.
pnpm tauri build --bundles deb --features embedded-libheif "${unsigned[@]}"
pnpm tauri build --bundles appimage --features embedded-libheif --config src-tauri/tauri.appimage.conf.json "${unsigned[@]}"
ls -la "$root/target/release/bundle/deb" "$root/target/release/bundle/appimage"
