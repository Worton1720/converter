#!/usr/bin/env bash
# Подготовка выпуска в ветке dev: поднимает версию во всех файлах проекта, проверяет
# CHANGELOG.md и делает коммит «Выпуск X.Y.Z». Сам выпуск (тег vX.Y.Z, пакеты, страница
# на GitHub) создаёт GitHub Actions после слияния dev → master — см. .github/workflows/release.yml.
#
#   tools/release.sh 0.2.0
set -euo pipefail

version="${1:?укажите версию, например 0.2.0}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || { echo "версия должна быть вида 1.2.3 или 1.2.3-beta.1" >&2; exit 2; }
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

[[ "$(git branch --show-current)" == dev ]] || { echo "выпуск готовится в ветке dev: git switch dev" >&2; exit 1; }
[[ -z "$(git status --porcelain)" ]] || { echo "есть незакоммиченные изменения — сначала закоммитьте их" >&2; exit 1; }
git fetch -q --tags origin || true
git rev-parse -q --verify "refs/tags/v$version" >/dev/null && { echo "версия $version уже выпущена" >&2; exit 1; }
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
git diff --cached --quiet || git commit -m "Выпуск $version"
cat <<MSG
готово: версия $version в dev. Дальше:
  git push
  gh pr create --base master --head dev --fill    # или «Compare & pull request» на GitHub
После зелёных проверок — слить запрос; выпуск v$version появится сам.
MSG
