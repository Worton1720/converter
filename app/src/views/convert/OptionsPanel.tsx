import { open } from "@tauri-apps/plugin-dialog";
import { Bookmark, Trash2 } from "lucide-react";
import { useState, type ReactNode } from "react";
import { Advanced } from "../../components/Advanced";
import { Button, Dialog, Field, IconButton, Select, Slider, Step, Switch } from "../../components/ui";
import { useStore } from "../../lib/store";
import type {
  ConflictPolicy,
  ConvertOptions,
  HwPreference,
  ImageFormat,
  ManualVideo,
  MetadataPolicy,
  OutputDir,
  QualityProfile,
  SourcePolicy,
  VideoMode,
} from "../../lib/types";

function Group({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="space-y-3 border-t border-line px-5 py-4">
      <h3 className="text-[18px] font-semibold">{title}</h3>
      {children}
    </section>
  );
}

const videoModeKey = (m: VideoMode) =>
  typeof m === "string" ? m : "Webm" in m ? `Webm${m.Webm}` : "Audio" in m ? `Audio${m.Audio}` : "Manual";
const DEFAULT_MANUAL: ManualVideo = { codec: "H264", container: "Mp4", crf: null, short_side: null, fps: null };
const VIDEO_MODES: { value: string; mode: VideoMode; label: string; hint: string; summary: string }[] = [
  {
    value: "Compat",
    mode: "Compat",
    label: "Видео для любых устройств",
    hint: "Откроется на телефоне, компьютере и телевизоре",
    summary: "Видео можно будет открыть на любом устройстве.",
  },
  {
    value: "Compact",
    mode: "Compact",
    label: "Видео поменьше",
    hint: "Файл меньше, но на старых компьютерах может не открыться",
    summary: "Видео станут меньше по размеру.",
  },
  { value: "WebmAv1", mode: { Webm: "Av1" }, label: "Для сайтов (WebM)", hint: "Для загрузки на сайты и в браузер", summary: "Видео подготовятся для сайтов." },
  { value: "WebmVp9", mode: { Webm: "Vp9" }, label: "Для старых сайтов (WebM VP9)", hint: "Делается долго", summary: "Видео подготовятся для старых сайтов." },
  { value: "Mkv", mode: "Mkv", label: "Без изменений (MKV)", hint: "Быстро, качество не меняется", summary: "Видео не изменятся, сменится только формат файла." },
  {
    value: "AudioMp3",
    mode: { Audio: "Mp3" },
    label: "Только звук (MP3)",
    hint: "Например, песня или голос из видео. Открывается везде",
    summary: "Из видео сохранится только звук в формате MP3.",
  },
  {
    value: "AudioM4a",
    mode: { Audio: "M4a" },
    label: "Только звук (M4A)",
    hint: "Звучит лучше MP3 при том же размере",
    summary: "Из видео сохранится только звук в формате M4A.",
  },
  {
    value: "Manual",
    mode: { Manual: DEFAULT_MANUAL },
    label: "Свои настройки (для опытных)",
    hint: "Кодек, качество, размер кадра — в расширенных настройках",
    summary: "Видео перекодируются по вашим настройкам.",
  },
];

const MANUAL_CODECS: { value: ManualVideo["codec"]; label: string; hint?: string }[] = [
  { value: "H264", label: "H.264", hint: "Совместимость везде" },
  { value: "Hevc", label: "HEVC (H.265)", hint: "Вдвое меньше H.264" },
  { value: "Av1", label: "AV1", hint: "Ещё меньше, нужен современный плеер" },
  { value: "Vp9", label: "VP9", hint: "Только процессором, медленно" },
];

const IMAGE_FORMATS: { value: ImageFormat; label: string; hint: string }[] = [
  { value: "Jpeg", label: "Обычные фото (JPEG)", hint: "Открываются на любом устройстве" },
  { value: "Png", label: "Без потерь качества (PNG)", hint: "Файлы в 8–10 раз больше" },
  { value: "Webp", label: "Маленькие фото (WebP)", hint: "Меньше обычных, но открываются не везде" },
];

const outputPath = (o: OutputDir) => (typeof o === "string" ? null : "Into" in o ? o.Into : o.IntoKeepTree);
const keepTree = (o: OutputDir) => typeof o === "object" && "IntoKeepTree" in o;

/** Что получится — одной фразой, без терминов. */
function summary(o: ConvertOptions): string {
  const image = {
    Jpeg: "Фото сохранятся в обычном формате JPEG.",
    Png: "Фото сохранятся без потерь качества (PNG).",
    Webp: "Фото станут меньше по размеру (WebP).",
  }[o.image.format];
  const video = VIDEO_MODES.find((m) => m.value === videoModeKey(o.video.mode))!.summary;
  const where = typeof o.output === "string" ? "рядом с исходными файлами" : "в выбранной папке";
  const sources =
    o.sources === "Keep"
      ? "исходники останутся на месте"
      : o.sources === "Trash"
        ? "исходники отправятся в корзину"
        : "исходники будут удалены навсегда";
  return `${image} ${video} Результат появится ${where}, ${sources}.`;
}

/** Имя результата по шаблону — для подсказки под полем. Дата — пример. */
function namePreview(o: ConvertOptions, sourceName = "IMG_0822.HEIC"): string {
  const dot = sourceName.lastIndexOf(".");
  const stem = dot > 0 ? sourceName.slice(0, dot) : sourceName;
  const srcExt = dot > 0 ? sourceName.slice(dot + 1) : "";
  const ext = { Jpeg: "jpg", Png: "png", Webp: "webp" }[o.image.format];
  const name =
    o.name_template
      .replaceAll("{name}", stem)
      .replaceAll("{ext}", srcExt)
      .replaceAll("{n}", "1")
      .replaceAll("{date}", "2025-10-25_15-11-48")
      .replace(/[<>:"/\\|?*]/g, "_") || "_";
  return `${name}.${ext}`;
}

/** Расширенные настройки, отличные от значений по умолчанию, — чтобы их было видно в свёрнутом блоке. */
function changedAdvanced(o: ConvertOptions): string[] {
  const out: string[] = [];
  if (o.sources === "Delete") out.push("удаление исходников навсегда");
  if (o.image.format !== "Png" && o.image.quality !== 90) out.push(`качество фото ${o.image.quality}`);
  if (o.image.format === "Png" && o.image.png_compression !== "Fast") out.push("сжатие PNG");
  if (o.image.format === "Png" && o.image.png_16bit) out.push("PNG 16 бит");
  if (o.image.max_side) out.push(`размер до ${o.image.max_side} px`);
  if (o.image.to_srgb) out.push("цвета в sRGB");
  if (o.video.quality !== "Balanced") out.push("качество видео");
  if (o.video.hw !== "Auto") out.push("видеокарта");
  if (o.video.audio_bitrate_kbps !== 192) out.push("качество звука");
  if (o.image.format === "Webp" && o.image.fit_webp_limit) out.push("уменьшение больших фото");
  if (keepTree(o.output)) out.push("структура подпапок");
  if (o.name_template !== "{name}") out.push("имя файла");
  if (o.conflict !== "Suffix") out.push("совпадение имён");
  if (o.metadata !== "Keep") out.push("метаданные");
  if (!o.preserve_mtime) out.push("дата изменения");
  return out;
}

/** Число в списке: «как в исходнике» хранится как `null`. */
const SAME = "same";
const numOrNull = (v: string) => (v === SAME ? null : Number(v));
const numKey = (n: number | null) => (n === null ? SAME : String(n));

/** Ручной режим видео (FR-3.1) — только в расширенных настройках. */
function ManualVideoFields({ value, onChange }: { value: ManualVideo; onChange: (m: ManualVideo) => void }) {
  const webmOk = value.codec === "Av1" || value.codec === "Vp9";
  return (
    <>
      <Field label="Кодек">
        <Select<ManualVideo["codec"]>
          value={value.codec}
          onChange={(codec) => {
            const ok = codec === "Av1" || codec === "Vp9";
            onChange({ ...value, codec, container: value.container === "Webm" && !ok ? "Mp4" : value.container });
          }}
          options={MANUAL_CODECS}
        />
      </Field>
      <Field label="Формат файла">
        <Select<ManualVideo["container"]>
          value={value.container}
          onChange={(container) => onChange({ ...value, container })}
          options={[
            { value: "Mp4", label: "MP4", hint: "Открывается везде" },
            { value: "Mkv", label: "MKV", hint: "Сохраняет все дорожки звука и субтитры" },
            ...(webmOk ? [{ value: "Webm" as const, label: "WebM", hint: "Для сайтов и браузера" }] : []),
          ]}
        />
      </Field>
      <Field label="Качество (CRF)" hint="Меньше число — лучше качество и больше файл.">
        <Select
          value={numKey(value.crf)}
          onChange={(v) => onChange({ ...value, crf: numOrNull(v) })}
          options={[
            { value: SAME, label: "По профилю ниже" },
            { value: "16", label: "16 — почти без потерь" },
            { value: "20", label: "20" },
            { value: "23", label: "23 — обычно для H.264" },
            { value: "26", label: "26" },
            { value: "28", label: "28 — обычно для HEVC" },
            { value: "32", label: "32 — обычно для AV1" },
            { value: "36", label: "36" },
            { value: "40", label: "40 — маленький файл" },
          ]}
        />
      </Field>
      <Field label="Размер кадра" hint="Только уменьшение; вертикальные видео тоже учитываются.">
        <Select
          value={numKey(value.short_side)}
          onChange={(v) => onChange({ ...value, short_side: numOrNull(v) })}
          options={[
            { value: SAME, label: "Как в исходнике" },
            { value: "2160", label: "4K (2160)" },
            { value: "1440", label: "1440" },
            { value: "1080", label: "Full HD (1080)" },
            { value: "720", label: "HD (720)" },
            { value: "480", label: "480" },
            { value: "360", label: "360" },
          ]}
        />
      </Field>
      <Field label="Кадров в секунду">
        <Select
          value={numKey(value.fps)}
          onChange={(v) => onChange({ ...value, fps: numOrNull(v) })}
          options={[
            { value: SAME, label: "Как в исходнике" },
            { value: "60", label: "60" },
            { value: "50", label: "50" },
            { value: "30", label: "30" },
            { value: "25", label: "25" },
            { value: "24", label: "24" },
            { value: "15", label: "15" },
          ]}
        />
      </Field>
    </>
  );
}

export function OptionsPanel() {
  const options = useStore((s) => s.settings?.convert);
  const info = useStore((s) => s.info);
  const presets = useStore((s) => s.settings?.presets ?? []);
  const scanRecursive = useStore((s) => s.settings?.scan_recursive ?? true);
  const scanHidden = useStore((s) => s.settings?.scan_hidden ?? false);
  // Для примера имени — первое фото списка: расширение результата берётся из формата фото.
  const firstName = useStore((s) => {
    const p = s.order.find((x) => s.rows[x]?.item.kind === "image");
    return p ? s.rows[p].item.name : undefined;
  });
  const updateConvert = useStore((s) => s.updateConvert);
  const updateSettings = useStore((s) => s.updateSettings);
  const runActive = useStore((s) => s.run.active);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [presetName, setPresetName] = useState<string | null>(null);

  if (!options) return null;
  const set = (fn: (o: ConvertOptions) => ConvertOptions) => updateConvert(fn);
  const img = options.image;
  const lossy = img.format !== "Png";
  const separate = typeof options.output !== "string";

  const backends = [...new Set(info?.hw.map((h) => h.backend) ?? [])];
  const hwKey = typeof options.video.hw === "object" ? options.video.hw.Only : options.video.hw;
  const mode = options.video.mode;
  const manual = typeof mode === "object" && "Manual" in mode ? mode.Manual : null;
  const setManual = (m: ManualVideo) => set((o) => ({ ...o, video: { ...o.video, mode: { Manual: m } } }));
  const autoLabel = backends.length ? `Автоматически (${backends[0]})` : "Автоматически (не найдена)";

  const pickDir = async () => {
    const dir = await open({ directory: true, title: "Папка для результатов" });
    if (typeof dir === "string") {
      set((o) => ({ ...o, output: keepTree(o.output) ? { IntoKeepTree: dir } : { Into: dir } }));
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <fieldset disabled={runActive} className="min-h-0 flex-1 overflow-y-auto disabled:opacity-60">
        <div className="space-y-5 px-5 py-5">
          <Step n={2} title="Что сделать" />
          <p className="rounded-lg bg-accent-soft px-4 py-3 text-[16px] leading-relaxed">{summary(options)}</p>

          <Field label="Во что превратить фото">
            <Select<ImageFormat>
              value={img.format}
              onChange={(format) => set((o) => ({ ...o, image: { ...o.image, format } }))}
              options={IMAGE_FORMATS}
            />
          </Field>

          <Field label="Во что превратить видео">
            <Select
              value={videoModeKey(options.video.mode)}
              onChange={(v) => {
                const mode = VIDEO_MODES.find((m) => m.value === v)!.mode;
                set((o) => ({ ...o, video: { ...o.video, mode } }));
              }}
              options={VIDEO_MODES.map(({ value, label, hint }) => ({ value, label, hint }))}
            />
          </Field>
          {manual && (
            <p className="-mt-2 text-[14px] text-muted">
              Кодек, качество и размер кадра — в «Расширенных настройках» ниже, раздел «Видео».
            </p>
          )}

          <Field
            label="Куда сохранить"
            hint={
              separate && (
                <span className="flex items-start justify-between gap-2">
                  <span className="break-all">{outputPath(options.output)}</span>
                  <button className="shrink-0 text-accent hover:underline" onClick={() => void pickDir()}>
                    Изменить
                  </button>
                </span>
              )
            }
          >
            <Select
              value={separate ? "Into" : "BesideSource"}
              onChange={(v) => {
                if (v === "BesideSource") set((o) => ({ ...o, output: "BesideSource" }));
                else void pickDir();
              }}
              options={[
                { value: "BesideSource", label: "Рядом с исходными файлами" },
                { value: "Into", label: "В отдельную папку…" },
              ]}
            />
          </Field>

          <Field label="Что сделать с исходными файлами" hint="Исходный файл убирается только после проверки результата.">
            <Select<SourcePolicy>
              value={options.sources}
              onChange={(sources) => set((o) => ({ ...o, sources }))}
              options={[
                { value: "Keep", label: "Оставить на месте" },
                { value: "Trash", label: "Убрать в корзину", hint: "Из корзины их можно вернуть" },
                // Безвозвратное удаление включается только в расширенных настройках (ТЗ 8.5).
                ...(options.sources === "Delete"
                  ? [{ value: "Delete" as const, label: "Удалять навсегда", hint: "Включено в расширенных настройках" }]
                  : []),
              ]}
            />
          </Field>
        </div>

        <Advanced
          id="convert"
          changed={[
            ...changedAdvanced(options),
            ...(!scanRecursive ? ["без вложенных папок"] : []),
            ...(scanHidden ? ["скрытые файлы"] : []),
          ]}
          inset="px-5"
          className="border-t border-line"
        >
          <Group title="Исходные файлы">
            <Switch
              checked={options.sources === "Delete"}
              onChange={(v) => (v ? setConfirmDelete(true) : set((o) => ({ ...o, sources: "Trash" })))}
              label="Удалять исходники навсегда, без корзины"
              hint="Вернуть их будет нельзя"
            />
          </Group>
          <Group title="Фото">
            {lossy ? (
              <Field label={`Качество: ${img.quality}`} hint="90 — почти без видимых потерь. Ниже — файлы меньше.">
                <Slider
                  label="Качество"
                  value={img.quality}
                  min={50}
                  max={100}
                  onChange={(quality) => set((o) => ({ ...o, image: { ...o.image, quality } }))}
                />
              </Field>
            ) : (
              <>
                <Field label="Сжатие PNG">
                  <Select<"Fast" | "Compact">
                    value={img.png_compression}
                    onChange={(png_compression) => set((o) => ({ ...o, image: { ...o.image, png_compression } }))}
                    options={[
                      { value: "Fast", label: "Быстрое" },
                      { value: "Compact", label: "Компактное", hint: "Медленнее, файл чуть меньше" },
                    ]}
                  />
                </Field>
                <Switch
                  checked={img.png_16bit}
                  onChange={(png_16bit) => set((o) => ({ ...o, image: { ...o.image, png_16bit } }))}
                  label="16 бит на канал"
                  hint="Для 10-битных HEIC: больше оттенков, файл вдвое больше"
                />
              </>
            )}
            <Field label="Размер">
              <Select
                value={String(img.max_side ?? 0)}
                onChange={(v) => set((o) => ({ ...o, image: { ...o.image, max_side: Number(v) || null } }))}
                options={[
                  { value: "0", label: "Как в оригинале" },
                  { value: "4096", label: "Не больше 4096 px" },
                  { value: "2560", label: "Не больше 2560 px" },
                  { value: "1920", label: "Не больше 1920 px", hint: "Хватает для экрана и мессенджеров" },
                  { value: "1280", label: "Не больше 1280 px" },
                ]}
              />
            </Field>
            <Switch
              checked={img.to_srgb}
              onChange={(to_srgb) => set((o) => ({ ...o, image: { ...o.image, to_srgb } }))}
              label="Перевести цвета в sRGB"
              hint="Если программа показывает фото iPhone тусклыми"
            />
            {img.format === "Webp" && (
              <Switch
                checked={img.fit_webp_limit}
                onChange={(fit_webp_limit) => set((o) => ({ ...o, image: { ...o.image, fit_webp_limit } }))}
                label="Уменьшать очень большие фото"
                hint="WebP не умеет больше 16 383 точек по стороне — без этого такие фото не сохранятся"
              />
            )}
          </Group>

          <Group title="Видео">
            {manual && <ManualVideoFields value={manual} onChange={setManual} />}
            <Field label={manual ? "Скорость кодирования" : "Качество"}>
              <Select<QualityProfile>
                value={options.video.quality}
                onChange={(quality) => set((o) => ({ ...o, video: { ...o.video, quality } }))}
                options={[
                  { value: "Fast", label: "Быстро", hint: "Файл больше, кодирование быстрее" },
                  { value: "Balanced", label: "Сбалансированно" },
                  { value: "High", label: "Высокое", hint: "Медленнее" },
                ]}
              />
            </Field>
            <Field label="Видеокарта" hint="С видеокартой видео кодируется в разы быстрее.">
              <Select
                value={hwKey}
                onChange={(v) => {
                  const hw: HwPreference = v === "Auto" || v === "Off" ? v : { Only: v as never };
                  set((o) => ({ ...o, video: { ...o.video, hw } }));
                }}
                options={[
                  { value: "Auto", label: autoLabel },
                  ...backends.map((b) => ({ value: b[0] + b.slice(1).toLowerCase(), label: `Только ${b}` })),
                  { value: "Off", label: "Не использовать", hint: "Кодировать процессором" },
                ]}
              />
            </Field>
            <Field label="Качество звука">
              <Select
                value={String(options.video.audio_bitrate_kbps)}
                onChange={(v) => set((o) => ({ ...o, video: { ...o.video, audio_bitrate_kbps: Number(v) } }))}
                options={[
                  { value: "128", label: "Обычное (128 кбит/с)" },
                  { value: "192", label: "Хорошее (192 кбит/с)" },
                  { value: "256", label: "Высокое (256 кбит/с)" },
                  { value: "320", label: "Максимальное (320 кбит/с)" },
                ]}
              />
            </Field>
          </Group>

          <Group title="Имена и папки">
            <Switch
              checked={keepTree(options.output)}
              disabled={!separate}
              onChange={(v) => {
                const dir = outputPath(options.output);
                if (dir) set((o) => ({ ...o, output: v ? { IntoKeepTree: dir } : { Into: dir } }));
              }}
              label="Повторить структуру подпапок"
              hint={separate ? "В папке результата появятся те же подпапки" : "Доступно при сохранении в отдельную папку"}
            />
            <Field
              label="Имя файла"
              hint={
                <>
                  <span className="block">
                    Получится: <span className="font-medium text-text">{namePreview(options, firstName)}</span>
                  </span>
                  <span className="block">{"{name}"} — имя исходного файла, {"{date}"} — дата съёмки, {"{n}"} — номер по порядку</span>
                </>
              }
            >
              <input
                value={options.name_template}
                onChange={(e) => set((o) => ({ ...o, name_template: e.target.value }))}
                spellCheck={false}
                className="h-11 w-full rounded-lg border border-line-strong bg-surface px-3 text-[16px]"
              />
            </Field>
            <Field label="Если файл с таким именем уже есть">
              <Select<ConflictPolicy>
                value={options.conflict}
                onChange={(conflict) => set((o) => ({ ...o, conflict }))}
                options={[
                  { value: "Suffix", label: "Добавить номер" },
                  { value: "Overwrite", label: "Заменить" },
                  { value: "Skip", label: "Пропустить" },
                ]}
              />
            </Field>
          </Group>

          <Group title="Сведения о файле">
            <Field label="Метаданные" hint="Дата съёмки, камера, геопозиция.">
              <Select<MetadataPolicy>
                value={options.metadata}
                onChange={(metadata) => set((o) => ({ ...o, metadata }))}
                options={[
                  { value: "Keep", label: "Сохранить все" },
                  { value: "StripGps", label: "Убрать геопозицию" },
                  { value: "StripAll", label: "Убрать все" },
                ]}
              />
            </Field>
            <Switch
              checked={options.preserve_mtime}
              onChange={(preserve_mtime) => set((o) => ({ ...o, preserve_mtime }))}
              label="Сохранить дату изменения"
            />
          </Group>

          <Group title="Добавление файлов">
            <Switch
              checked={scanRecursive}
              onChange={(scan_recursive) => updateSettings((s) => ({ ...s, scan_recursive }))}
              label="Искать во вложенных папках"
            />
            <Switch
              checked={scanHidden}
              onChange={(scan_hidden) => updateSettings((s) => ({ ...s, scan_hidden }))}
              label="Добавлять скрытые файлы"
            />
          </Group>

          <Group title="Пресеты">
            <p className="text-[14px] text-muted">Пресет запоминает все настройки с этой панели.</p>
            {presets.map((p) => (
              <div key={p.name} className="flex items-center gap-2 rounded-lg border border-line py-1 pr-1 pl-3">
                <span className="min-w-0 flex-1 truncate text-[16px]">{p.name}</span>
                <Button variant="ghost" onClick={() => set(() => structuredClone(p.convert))}>
                  Применить
                </Button>
                <IconButton
                  label="Удалить"
                  onClick={() => updateSettings((s) => ({ ...s, presets: s.presets.filter((x) => x.name !== p.name) }))}
                >
                  <Trash2 className="size-4" />
                </IconButton>
              </div>
            ))}
            <Button onClick={() => setPresetName("")}>
              <Bookmark className="size-4" />
              Сохранить как пресет
            </Button>
          </Group>
        </Advanced>
      </fieldset>

      <Dialog
        open={confirmDelete}
        onOpenChange={setConfirmDelete}
        title="Удалять исходники навсегда?"
        width={440}
        footer={
          <>
            <Button variant="primary" onClick={() => setConfirmDelete(false)}>
              Не удалять
            </Button>
            <Button
              variant="danger"
              onClick={() => {
                set((o) => ({ ...o, sources: "Delete" }));
                setConfirmDelete(false);
              }}
            >
              Удалять навсегда
            </Button>
          </>
        }
      >
        <p className="text-[16px] leading-relaxed">
          Исходный файл будет удалён без корзины сразу после того, как результат пройдёт проверку. Вернуть его будет
          нельзя. Если сомневаетесь, выберите «Отправить в корзину».
        </p>
      </Dialog>

      <Dialog
        open={presetName !== null}
        onOpenChange={(v) => !v && setPresetName(null)}
        title="Новый пресет"
        width={400}
        footer={
          <>
            <Button variant="ghost" onClick={() => setPresetName(null)}>
              Отмена
            </Button>
            <Button
              variant="primary"
              disabled={!presetName?.trim()}
              onClick={() => {
                const name = presetName!.trim();
                updateSettings((s) => ({
                  ...s,
                  presets: [...s.presets.filter((p) => p.name !== name), { name, convert: structuredClone(s.convert) }],
                }));
                setPresetName(null);
              }}
            >
              Сохранить
            </Button>
          </>
        }
      >
        <Field label="Название" hint="Сохранятся все параметры с этой панели.">
          <input
            autoFocus
            value={presetName ?? ""}
            onChange={(e) => setPresetName(e.target.value)}
            placeholder="Например, для Telegram"
            className="h-8 w-full rounded-md border border-line-strong bg-surface px-2.5 text-[16px]"
          />
        </Field>
      </Dialog>
    </div>
  );
}
