#!/usr/bin/env bash
# Автотесты интерфейса: отладочная сборка выполняет app/tests/ui/scenarios.js (`--dev-test`).
# Настройки, кэш, журналы и корзина — во временной папке: данные пользователя не трогаются.
# Экран: xvfb-run (CI) или невидимый kwin_wayland --virtual (KDE); иначе окно откроется на экране.
#
#   tools/ui-test.sh              # нужны pnpm, ffmpeg, собранный фронтенд-сервер Vite
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
work="$root/target/ui-test"
# Службы прошлого вложенного сеанса (kwalletd) могут ещё дописывать в папку — повторить.
rm -rf "$work" 2>/dev/null || { sleep 3; rm -rf "$work"; }
mkdir -p "$work"/{config,cache,state/converter,data,files/phone,files/copies,files/crash}

# Тестовые файлы: фото с телефона (HEIC + PNG) и точная копия для поиска одинаковых.
cp crates/core/tests/fixtures/sample.heic "$work/files/phone/IMG_A.HEIC"
ffmpeg -v error -f lavfi -i "gradients=s=480x360:c0=0x0f766e:c1=0xfde68a:seed=7" -frames:v 1 \
  "$work/files/phone/IMG_B.png"
cp crates/core/tests/fixtures/sample.heic "$work/files/copies/Отпуск.heic"
# «Сбой»: незакрытая запись истории с одним несделанным файлом — программа должна предложить доделать.
ffmpeg -v error -f lavfi -i "gradients=s=320x240:c0=0x312e81:c1=0xfbcfe8:seed=3" -frames:v 1 \
  "$work/files/crash/IMG_C.png"
printf '{"entries":[{"id":1700000000000,"finished_ms":null,"action":{"Convert":{"options":{},"pending":["%s"],"skipped":0,"failed":0}},"changes":[],"undone":false}]}' \
  "$work/files/crash/IMG_C.png" >"$work/state/converter/history.json"
cp crates/core/tests/fixtures/sample.heic "$work/files/copies/Отпуск (копия).heic"

# Без проверки обновлений: тест не должен ходить в сеть.
mkdir -p "$work/config/converter"
printf 'check_updates = false\n' >"$work/config/converter/settings.toml"

export XDG_CONFIG_HOME="$work/config" XDG_CACHE_HOME="$work/cache" \
  XDG_STATE_HOME="$work/state" XDG_DATA_HOME="$work/data" \
  CONVERTER_TEST_DATA="$work/files"

# Отладочная сборка берёт интерфейс с сервера разработки Vite.
[[ -d app/node_modules ]] || pnpm --dir app install --frozen-lockfile
pnpm --dir app exec vite --port 1420 --strictPort >"$work/vite.log" 2>&1 &
vite=$!
trap 'kill $vite 2>/dev/null || true' EXIT
cargo build -q -p converter-app
for _ in $(seq 1 60); do
  curl -sf http://localhost:1420 >/dev/null && break
  sleep 1
done

app=(target/debug/converter-app --dev-test app/tests/ui/scenarios.js)
status=0
if command -v xvfb-run >/dev/null; then
  xvfb-run -a -s "-screen 0 1600x1000x24" timeout 300 "${app[@]}" | tee "$work/result.log" || status=$?
elif command -v kwin_wayland >/dev/null; then
  socket="ui-test-$$"
  # Своя шина D-Bus: в общей вложенный KWin регистрирует свои горячие клавиши
  # в kglobalaccel настоящего, а при выходе снимает их — у пользователя
  # перестают работать Alt+Tab и прочие сочетания KWin до перезахода.
  # Своя папка XDG_RUNTIME_DIR: службы вложенного сеанса (сокеты Wayland, at-spi/bus_0)
  # иначе пишут в /run/user/<uid> поверх настоящих — у пользователя ломалась служба
  # специальных возможностей AT-SPI, и программы запускались с задержкой.
  runtime="$work/runtime"
  mkdir -p -m 700 "$runtime"
  XDG_RUNTIME_DIR="$runtime" NO_AT_BRIDGE=1 dbus-run-session -- \
    kwin_wayland --virtual --xwayland --width 1600 --height 1000 --socket "$socket" \
    --exit-with-session "$root/tools/ui-test-app.sh" >"$work/kwin.log" 2>&1 || status=$?
  cat "$work/result.log" 2>/dev/null || true
  grep -q "итог: всё прошло" "$work/result.log" 2>/dev/null || status=1
else
  echo "нет xvfb-run и kwin_wayland — окно теста откроется на экране"
  timeout 300 "${app[@]}" | tee "$work/result.log" || status=$?
fi
# Проверка по самим файлам: «Вернуть как было» вернуло копию и убрало результаты.
if [[ $status -eq 0 ]]; then
  for f in "phone/IMG_A.HEIC" "phone/IMG_B.png" "copies/Отпуск.heic" "copies/Отпуск (копия).heic"; do
    [[ -f "$work/files/$f" ]] || { echo "ui-test: нет файла $f после «Вернуть как было»"; status=1; }
  done
  [[ -f "$work/files/crash/IMG_C.jpg" ]] || { echo "ui-test: «Доделать» не сделал crash/IMG_C.jpg"; status=1; }
  for f in "phone/IMG_A.jpg" "phone/IMG_B.jpg"; do
    [[ ! -e "$work/files/$f" ]] || { echo "ui-test: результат $f не убран при отмене"; status=1; }
  done
fi
[[ $status -eq 0 ]] && grep -q "итог: всё прошло" "$work/result.log" || {
  echo "АВТОТЕСТЫ ИНТЕРФЕЙСА НЕ ПРОШЛИ (подробности: $work)" >&2
  exit 1
}
