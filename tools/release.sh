#!/usr/bin/env bash
# Подготовка выпуска: поднимает версию во всех файлах проекта, проверяет CHANGELOG.md,
# делает коммит «Выпуск X.Y.Z» и тег vX.Y.Z. Публикует GitHub Actions после
#   git push --follow-tags
#
#   tools/release.sh 0.2.0
set -euo pipefail

version="${1:?укажите версию, например 0.2.0}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || { echo "версия должна быть вида 1.2.3 или 1.2.3-beta.1" >&2; exit 2; }
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

[[ -z "$(git status --porcelain)" ]] || { echo "есть незакоммиченные изменения — сначала закоммитьте их" >&2; exit 1; }
git rev-parse -q --verify "refs/tags/v$version" >/dev/null && { echo "тег v$version уже есть" >&2; exit 1; }
grep -q "^## \[$version\]" CHANGELOG.md || {
  echo "в CHANGELOG.md нет раздела «## [$version] — ГГГГ-ММ-ДД»: опишите изменения и запустите снова" >&2
  exit 1
}

# Версия — в [workspace.package] корневого Cargo.toml, package.json и tauri.conf.json.
sed -i -E "0,/^version = \"[^\"]+\"/s//version = \"$version\"/" Cargo.toml
for f in app/package.json app/src-tauri/tauri.conf.json; do
  sed -i -E "0,/\"version\": \"[^\"]+\"/s//\"version\": \"$version\"/" "$f"
done
cargo update --workspace --quiet

git add Cargo.toml Cargo.lock app/package.json app/src-tauri/tauri.conf.json
# Первый выпуск идёт с версией, уже записанной в файлах, — тогда коммитить нечего.
git diff --cached --quiet || git commit -m "Выпуск $version"
git tag -a "v$version" -m "Converter $version"
echo "готово: тег v$version. Опубликовать: git push --follow-tags"
