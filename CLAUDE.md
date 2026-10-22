# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Десктоп-приложение (Linux + Windows) для пакетной конвертации фото/видео (прежде всего iPhone: HEIC, HEVC/MOV) и поиска дубликатов. ТЗ — `docs/TZ.md` (номера FR-… в комментариях кода ссылаются на него; раздел 15 — статус этапов), бэклог — `tasks.md`. Интерфейс, комментарии и сообщения об ошибках — на русском.

**Главный пользователь — пожилой человек без опыта работы с ПК.** Любое изменение интерфейса проверять по критериям ТЗ 8.5: крупный текст (≥ 16 px) и цели нажатия (≥ 44 px), никаких действий только по наведению, двойному щелчку, правой кнопке или горячим клавишам, никаких терминов (кодек, HEVC, EXIF, HDR…) вне «Расширенных настроек», пошаговый сценарий, безопасный вариант по умолчанию, результат и ошибки — простыми словами. Это требование приоритетнее плотности интерфейса.

## Команды

Rust-workspace в корне (`crates/core`, `crates/cli`, `app/src-tauri`), фронтенд — `app/` (pnpm).

```bash
cargo build                                   # всё
cargo test                                    # модульные + интеграционные
cargo test -p converter-core dupes            # тесты по подстроке имени
cargo test -p converter-core --test integration cancel_mid_encode   # один интеграционный тест
cargo clippy --all-targets -- -D warnings     # так же, как в CI
cargo fmt --all --check

./target/debug/converter hw [--refresh]       # найденный ffmpeg и аппаратные кодировщики
./target/debug/converter convert <пути> --out DIR [--to jpeg|png|webp] [--video compat|compact|webm|mkv] [--load auto|low|…|N]
./target/debug/converter dupes <пути> [--action report|trash|move]

cd app && pnpm install
pnpm typecheck                                # tsc (TypeScript 7)
pnpm build                                    # нужен до `cargo build/clippy` для converter-app: generate_context! читает app/dist
pnpm tauri dev                                # GUI в режиме разработки
pnpm tauri build --no-bundle                  # релизный бинарник target/release/converter-app

tools/build-linux.sh                          # .deb + AppImage (качает ffmpeg, собирает с embedded-libheif)
tools/fetch-ffmpeg.sh linux|windows           # встроенный ffmpeg → app/src-tauri/resources/ffmpeg (зафиксированный выпуск BtbN + sha256)
tools/release.sh X.Y.Z                        # в dev: версия во всех файлах + коммит; выпуск — после слияния dev → master
tools/smoke-test.sh target/release/converter   # дымовой тест собранной CLI (ffmpeg в PATH)
tools/ui-test.sh                              # автотесты интерфейса (app/tests/ui/scenarios.js), ~1 мин, окно невидимо
```

Ветки: разработка — в `dev`, `master` получает только слияния из `dev`. Хук `.githooks/pre-commit` (подключается `git config core.hooksPath .githooks`) гоняет fmt, typecheck, clippy и тесты и не пускает коммиты прямо в `master`. Push в `dev` и запросы на слияние проверяет `ci.yml`; слияние в `master` запускает `release.yml`: `ci.yml` целиком → пакеты → `tools/smoke-test.sh` на собранных программах → выпуск `vX.Y.Z`, если такого тега ещё нет (описание — раздел `## [X.Y.Z]` из `CHANGELOG.md`; версии в `Cargo.toml`, `package.json` и `tauri.conf.json` должны совпадать). Новая версия готовится в `dev` через `tools/release.sh X.Y.Z`.

Пакеты: `.deb` — базовый `tauri.conf.json` (системный ffmpeg по зависимости); AppImage — плюс `--config src-tauri/tauri.appimage.conf.json` (ресурс `ffmpeg/`); Windows — `tauri.windows.conf.json` подмешивается автоматически (ffmpeg и DLL libheif из vcpkg рядом с exe). Выпускные Linux-сборки идут с фичей `embedded-libheif` (libheif 1.23 статически, libde265 встроенный декодер, без плагинов): так AppImage не зависит от плагинов libheif системы, а сборка на Ubuntu 22.04 возможна. Встроенный ffmpeg ищется в `resource_dir/ffmpeg` (`Tools::locate_with`, передаётся из `setup`). Значки — только `icons/icon.ico` (Windows) и `icons/icon.png` 512 px (Linux: Tauri там берёт лишь PNG); собираются `tools/icon/make-icon.sh` из `tools/icon/icon.svg` и упрощённого `icon-small.svg` для 16–24 px. `tauri icon` не использовать — он снова разложит десяток файлов.

Интеграционные тесты (`crates/core/tests/integration.rs`) используют `data_media/` и ffmpeg из системы; без них тихо пропускаются.

Системные зависимости: ffmpeg/ffprobe ≥ 6.1 (в `PATH` или `tools/ffmpeg/`), libheif (Linux — пакет, Windows — `vcpkg install "libheif[core,libde265]:x64-windows"`, динамически из-за LGPL; без фичи `hevc` по умолчанию — она тянет GPL-кодировщик x265, а нам нужно только чтение), для GUI на Linux — WebKitGTK 4.1. lcms2 и libwebp собираются из исходников крейтами.

## Архитектура

**`converter-core`** — вся логика, без UI. CLI и Tauri-приложение — тонкие обёртки над ним.

- `engine.rs` — очередь конвертации. Диспетчер в своём потоке готовит задачи (`prepare`: план вывода, выбор операции, пропуски), запускает каждую в отдельном потоке, когда она помещается в пул токенов (`need`), и шлёт `events::Event` в crossbeam-канал. Job id = индекс элемента во входном списке + 1 — фронтенд и `cancel_item` на это опираются. Сбой HW-кодирования → задача возвращается в очередь с `force_sw`; нехватка места → пауза и возврат задачи в начало.
- `pool.rs` + `governor.rs` — уровень нагрузки. `Governor` раз в секунду меряет загрузку через `sysinfo`, алгоритм «Авто» (`AutoController`, чистая функция с тестами) выставляет бюджет потоков, лимит HW-сессий и приоритет в `TokenPool`. Задачи берут токены (`Permit`) и отдают при `Drop`. Пока в очереди есть фото, тяжёлому видео достаётся не больше ⌊бюджет/2⌋.
- `video.rs` — план видео (`plan`: перепаковка или кодирование, выбор кодировщика, аудио, HDR) и сборка аргументов ffmpeg (`build_args`, `quality_args` — единая таблица качества). `hw.rs` детектит кодировщики пробным кодированием, кэш в `~/.cache/converter/hw-detect.json`. `ffmpeg.rs` — поиск бинарников и запуск с `-progress pipe:1` и отменой.
- `photo/` — HEIC через libheif (`decode.rs`), остальное через `image`; масштаб и sRGB (`color.rs`, lcms2); кодирование PNG/JPEG/WebP (`encode.rs`); `webp_mux.rs` переупаковывает WebP в VP8X ради ICC/EXIF/XMP. `exif.rs` правит EXIF на месте (сброс Orientation, затирание GPS) без перекодирования блока.
- `fs_ops.rs` — инварианты безопасности данных: запись во временный `.<имя>.cvtmp-<pid>-<n>.<ext>` с журналом в state-dir и атомарное переименование; исходник удаляется только после проверки результата; корзина недоступна → файл остаётся (`SourceAction::KeptBecause`). `plan.rs` резервирует имена результатов до запуска задач.
- `dupes.rs` — точные копии: размер → частичный BLAKE3 (64 КБ с начала и конца) → полный BLAKE3, кэш `hashes.json`, лимит читателей на физический диск. Жёсткие ссылки схлопываются в представителя с `links`, удаляются все имена. `apply` проверяет инвариант «в группе остаётся файл» и «файл не изменился с момента сканирования». Группы `GroupKind::Formats` — одно фото в разных форматах (`find_same_photos`: размер в пикселях + дата из заголовка → dHash превью только у возможных пар → `cluster_same_photos`); у файлов в них разный размер, поэтому освобождаемое место считать по `FileEntry.size`, а не `DupGroup.size`. `auto_select(group, rule, prefer)` для таких групп оставляет формат `prefer`. Группы `GroupKind::Similar` — похожие фото (`find_photo_groups` → `cluster_similar`, по желанию: `Similarity`), `auto_select` их никогда не отмечает (FR-9.3). Отпечатки (dHash) кэшируются в `hashes.json` (поле `print`); `HashCache::entry` сбрасывает все хэши записи, если файл изменился.
- `sys.rs` — платформенный код: приоритеты (Linux — nice/ioprio на поток, Windows — классы приоритета, EcoQoS), привязка ffmpeg к приложению (`PR_SET_PDEATHSIG` + своя группа процессов / Job Object), батарея, тип диска.
- `scan.rs` — обход путей, тип файла по содержимому (`media.rs`, не по расширению), пробы видео и размеры фото (`inspect`), разметка Live Photo.
- `history.rs` — история действий (`history.json` в state-dir, 100 записей): по каждому файлу — исходник, результат и судьба исходника (`Fate`). Запись преобразования создаётся при старте со списком `pending` и закрывается по `RunFinished`; незакрытая запись — признак сбоя. Основа для «Вернуть как было» и продолжения после сбоя.
- Пути к файлам приложения — `paths.rs`; настройки — `settings.rs` (`settings.toml`, все структуры с `#[serde(default)]`; если меняется умолчание или смысл сохранённого значения — поднять `SETTINGS_VERSION` и добавить шаг в `Settings::migrate`, иначе у существующих пользователей останется старое значение); журнал на диске — `logfile.rs` (`converter.log` в state-dir/logs, ротация 5 × 10 МБ, пишется из `forward_events` приложения).

**`app/src-tauri`** — состояние `AppState` (список `MediaItem`, активный запуск, отчёт дубликатов, настройки), команды `#[tauri::command]` в `lib.rs`, DTO для фронтенда в `dto.rs`. События ядра пересылаются пачками раз в 100 мс (`convert:events`), статус регулятора — `governor`. Миниатюры — свой протокол `thumb://` (`thumb.rs` в ядре, кэш на диске).

Автообновление — `update.rs` (команды `check_update`, `install_update`, событие `update:progress`; переносная версия без типа пакета не обновляется сама) + `lib/update.ts` и `components/UpdateBanner.tsx`. Источник — `releases/latest/download/latest.json`, который собирает release.yml из `*.sig`; ключи платформ `linux-x86_64-appimage|deb`, `windows-x86_64-nsis` и запасные без типа. Подпись включает версию: проверять установку можно только обновлением с меньшей версии на большую (см. docs/LESSONS.md).

**`app/src`** — React 19 + Tailwind 4 + zustand. `lib/types.ts` повторяет serde-представление структур ядра (внешне тегированные enum: `"Auto"`, `{ Only: "Vaapi" }`, `{ Webm: "Av1" }`) — при изменении Rust-типов править и его. `lib/store.ts` — единое состояние, разбор пачек событий (`applyEvents`), отложенное сохранение настроек. Базовые настройки на виду, редкие — в `components/Advanced.tsx`.

## Неочевидное

Полный список встреченных проблем с причинами и решениями — [docs/LESSONS.md](docs/LESSONS.md): смотреть перед отладкой странного поведения, дописывать при каждой новой находке. Ниже — самое важное.

- WebKitGTK не грузит `<img src="thumb://…">` со страницы другого источника, а `fetch` с CORS проходит: миниатюры идут через `lib/thumbs.ts` (fetch → blob-URL), ответы протокола содержат `Access-Control-Allow-Origin`. CSP в `tauri.conf.json` в dev-режиме не применяется — проверять её только релизной сборкой.
- libx265 в ffmpeg 9 игнорирует выходные `-color_trc/-color_primaries`: цвет задаётся кадрам фильтром `setparams` (см. `build_args`).
- У MOV и MP4 одинаковый `format_name` в ffprobe — различает только тег `major_brand`.
- libheif на Windows не открывает пути вне ANSI: HEIC читается в память и `read_from_bytes`.
- Windows-код (`cfg(windows)`) локально не компилируется (нет Windows-таргета) — проверяется CI `windows-latest`.
- Автотесты интерфейса: отладочная сборка с `--dev-test <js>` выполняет сценарий после загрузки страницы (`devtest.rs`), итоги — `dev_report`/`dev_exit`. `tools/ui-test.sh` изолирует XDG-папки (настройки, кэш, журналы, корзина), отключает проверку обновлений и запускает невидимо (xvfb-run в CI, `kwin_wayland --virtual` локально). Сценарий проверяет правила ТЗ 8.5 на каждом экране — новый экран или кнопку покрывать там же. Ждать смены экрана по тексту, а не сразу кликать: иначе кнопка находится на прежнем экране.
- Отладка GUI без ручных кликов: отладочная сборка при втором запуске `converter-app --dev-eval "<js>"` выполняет JS в окне (single-instance → `WebviewWindow::eval`); в dev есть `window.__store`. `CONVERTER_DEVTOOLS=1` открывает инспектор.
