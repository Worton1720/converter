#!/usr/bin/env bash
# Установка пакетов в CI с повторами: зеркало apt на машинах GitHub (azure.archive.ubuntu.com)
# время от времени застревает на больших пакетах (WebKit), и задание висело до предела времени
# (docs/LESSONS.md). Повторные попытки идут через другие зеркала.
#
#   tools/apt-install.sh пакет…
set -uo pipefail

# Загруженные пакеты — в ~/apt-archives: эту папку сохраняет и возвращает шаг «Кэш пакетов apt»
# в workflow, и следующий прогон ставит их без зеркала.
archives="$HOME/apt-archives"
mkdir -p "$archives/partial"
opts=(-o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30
  -o DPkg::Lock::Timeout=120 -o Dir::Cache::Archives="$archives" -o APT::Keep-Downloaded-Packages=true)

# apt пишет кэш от root — иначе actions/cache не сможет его прочитать и сохранить.
own_cache() { sudo chown -R "$(id -u):$(id -g)" "$archives"; }
# Попытка 2 — основной архив Ubuntu, 3 — ближайшее зеркало по списку Ubuntu.
mirrors=("" "http://archive.ubuntu.com/ubuntu" "mirror+http://mirrors.ubuntu.com/mirrors.txt")

use_mirror() {
  local url=$1
  for f in /etc/apt/sources.list /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources; do
    [[ -f "$f" ]] || continue
    sudo sed -i -E "s#https?://[a-z.]*archive\.ubuntu\.com/ubuntu/?#${url}#g; s#mirror\+http://mirrors\.ubuntu\.com/mirrors\.txt#${url}#g" "$f"
  done
}

for attempt in 1 2 3; do
  if [[ -n "${mirrors[$((attempt - 1))]}" ]]; then
    echo "apt-get: зеркало ${mirrors[$((attempt - 1))]}"
    use_mirror "${mirrors[$((attempt - 1))]}"
  fi
  if timeout 240 sudo apt-get "${opts[@]}" update &&
    timeout 600 sudo apt-get "${opts[@]}" install -y "$@"; then
    own_cache
    exit 0
  fi
  echo "::warning::apt-get: попытка $attempt не удалась, повторяю"
  sleep 15
done
own_cache
echo "::error::apt-get: пакеты не установились за 3 попытки"
exit 1
