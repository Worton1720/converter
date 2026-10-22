# Разработка

## Что нужно

- Rust 1.88+, Node.js 22+ и pnpm 11;
- ffmpeg и ffprobe 6.1+ в `PATH` (или в `tools/ffmpeg/`);
- libheif: на Linux — пакет (`libheif-dev`, `libheif`), на Windows — `vcpkg install "libheif[core,libde265]:x64-windows"` (без x265: он под GPL, а HEIC нужно только читать);
- для интерфейса на Linux — WebKitGTK 4.1 (`libwebkit2gtk-4.1-dev`);
- для выпускных сборок Linux — ещё cmake и libde265 (libheif собирается из исходников и вшивается в программу).

## Первый запуск

```bash
git config core.hooksPath .githooks   # проверки перед каждым коммитом
cd app && pnpm install
pnpm tauri dev                        # приложение в режиме разработки
```

Командная строка: `cargo run -p converter-cli -- convert <пути> --out <папка>`.

## Проверки

Перед коммитом хук `.githooks/pre-commit` запускает то же, что CI: `cargo fmt --check`, `pnpm typecheck`, `cargo clippy -D warnings` и `cargo test` (около 15 секунд, если всё уже собрано). Если `app/dist` нет, хук сначала соберёт фронтенд — без него не компилируется `converter-app`. Пропустить проверки в крайнем случае: `git commit --no-verify`.

Интеграционные тесты (`crates/core/tests/integration.rs`) берут образцы из `data_media/` — в репозиторий она не входит. Без неё эти тесты тихо пропускаются; положите туда свои HEIC и MOV с iPhone, чтобы их включить.

Windows-код (`cfg(windows)`) проверяется только в CI на `windows-latest`.

## Как устроен проект

- `crates/core` — вся логика без интерфейса: очередь конвертации, регулятор нагрузки, фото, видео (через ffmpeg), поиск копий.
- `crates/cli` — консольная программа `converter`.
- `app/src-tauri` — оболочка Tauri: команды, состояние, протокол миниатюр.
- `app/src` — интерфейс на React.

Подробнее — в [CLAUDE.md](CLAUDE.md) (архитектура и неочевидные места) и [docs/TZ.md](docs/TZ.md) (требования; номера FR-… в комментариях кода ссылаются на него).

## Правила интерфейса

Главный пользователь — пожилой человек без опыта работы с компьютером. Любое изменение интерфейса проверяйте по разделу 8.5 ТЗ: текст от 16 px, кнопки от 44 px, никаких действий только по наведению, двойному щелчку или правой кнопке, никаких терминов вне «Расширенных настроек», безопасный вариант — по умолчанию. Интерфейс, комментарии и сообщения — на русском.

## Выпуск

1. Опишите изменения в `CHANGELOG.md` под заголовком `## [X.Y.Z] — ГГГГ-ММ-ДД` (раздел попадёт в описание выпуска).
2. `tools/release.sh X.Y.Z` — поднимет версию в `Cargo.toml`, `package.json` и `tauri.conf.json`, сделает коммит и тег `vX.Y.Z`.
3. `git push --follow-tags` — GitHub Actions соберёт установщики для Windows и Linux и опубликует выпуск с контрольными суммами.

Версия с суффиксом (`1.0.0-beta.1`) публикуется как предварительная. Собрать пакеты без выпуска — ручной запуск workflow «Release» во вкладке Actions или локально `tools/build-linux.sh`.

## Значки

Исходники — `tools/icon/icon.svg` и упрощённый `icon-small.svg` для 16–24 px. После правки: `tools/icon/make-icon.sh`.
