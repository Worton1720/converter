#!/usr/bin/env bash
# Установка пакетов в CI с повторами: зеркала apt на машинах GitHub время от времени
# зависают, и задание висело на этом шаге до предела времени (docs/LESSONS.md).
#
#   tools/apt-install.sh пакет…
set -uo pipefail

opts=(-o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30
  -o DPkg::Lock::Timeout=120)
for attempt in 1 2 3; do
  if timeout 240 sudo apt-get "${opts[@]}" update &&
    timeout 600 sudo apt-get "${opts[@]}" install -y "$@"; then
    exit 0
  fi
  echo "::warning::apt-get: попытка $attempt не удалась, повторяю"
  sleep 15
done
echo "::error::apt-get: пакеты не установились за 3 попытки"
exit 1
