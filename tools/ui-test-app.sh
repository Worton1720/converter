#!/usr/bin/env bash
# Сессия для невидимого kwin_wayland из tools/ui-test.sh: запускает тест и пишет его вывод.
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
exec timeout 300 target/debug/converter-app --dev-test app/tests/ui/scenarios.js >"$root/target/ui-test/result.log" 2>&1
