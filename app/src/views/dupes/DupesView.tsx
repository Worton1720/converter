import { useVirtualizer } from "@tanstack/react-virtual";
import { listen } from "@tauri-apps/api/event";
import { desktopDir, downloadDir, pictureDir } from "@tauri-apps/api/path";
import { open, save } from "@tauri-apps/plugin-dialog";
import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import { ArrowLeft, FileDown, FolderOpen, FolderPlus, Square, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Advanced } from "../../components/Advanced";
import { Button, Checkbox, Dialog, Field, IconButton, ProgressBar, Select, Step, Switch } from "../../components/ui";
import { api } from "../../lib/api";
import { basename, bytes, date, dirname, files, plural } from "../../lib/format";
import { useStore } from "../../lib/store";
import { useThumb } from "../../lib/thumbs";
import type { DupAction, DupesProgress, GroupKind, KeepRule, KindFilter, PhotoKind } from "../../lib/types";
import { Thumb } from "../convert/FileTable";
import { CompareDialog } from "./Compare";

type FlatRow = { type: "group"; g: number } | { type: "file"; g: number; f: number };
type RuleKey = "Oldest" | "Newest" | "ShortestPath" | "PriorityFolders";
type ActionKey = "Trash" | "MoveTo" | "Delete";

const ruleKey = (r: KeepRule): RuleKey => (typeof r === "object" ? "PriorityFolders" : r);
const kindKey = (k: KindFilter) => (typeof k === "object" ? "Extensions" : k);
const splitList = (text: string) =>
  text
    .split(",")
    .map((x) => x.trim())
    .filter(Boolean);

/** Что оставлять у одного фото в разных форматах (FR-9.2). */
const FORMATS: { value: PhotoKind; label: string; hint: string }[] = [
  { value: "Jpeg", label: "JPEG", hint: "Открывается на любом компьютере и телефоне" },
  { value: "Heif", label: "HEIC", hint: "Как снимает iPhone: места меньше, но открывается не везде" },
  { value: "Png", label: "PNG", hint: "Без потери качества, файлы большие" },
  { value: "Webp", label: "WebP", hint: "Для сайтов и мессенджеров" },
];
const formatLabel = (k: PhotoKind) => FORMATS.find((f) => f.value === k)?.label ?? k;

const RULES: { value: RuleKey; label: string; hint?: string }[] = [
  { value: "Oldest", label: "Самый старый файл" },
  { value: "Newest", label: "Самый новый файл" },
  { value: "ShortestPath", label: "Файл с самым коротким путём" },
  { value: "PriorityFolders", label: "Файл из первой папки в списке", hint: "Порядок — как в шаге 1" },
];

function progressText(p: DupesProgress | null): { text: string; fraction: number | null } {
  if (!p) return { text: "Начинаю…", fraction: null };
  if ("Scanning" in p) return { text: `Просматриваю папки, найдено ${files(p.Scanning.found)}`, fraction: null };
  const h = p.Hashing;
  if (h.stage === 2) return { text: `Сравниваю файлы: ${h.done} из ${h.total}`, fraction: h.total ? h.done / h.total : 0 };
  if (h.stage === 4)
    return {
      text: `Ищу одно фото, сохранённое в разных форматах: ${h.done} из ${h.total}`,
      fraction: h.total ? h.done / h.total : 0,
    };
  return {
    text: `Проверяю совпадения подробно: ${bytes(h.bytes_done)} из ${bytes(h.bytes_total)}`,
    fraction: h.bytes_total ? h.bytes_done / h.bytes_total : 0,
  };
}

type Totals = { count: number; freed: number; groups: number };

/** Сколько будет убрано по текущим отметкам: всего и отдельно по типам групп. */
function useTotals() {
  const result = useStore((s) => s.dupes.result);
  const marks = useStore((s) => s.dupes.marks);
  return useMemo(() => {
    const zero = (): Totals => ({ count: 0, freed: 0, groups: 0 });
    const t = { all: zero(), Exact: zero(), Formats: zero() };
    result?.groups.forEach((g, gi) => {
      t[g.kind].groups += 1;
      t.all.groups += 1;
      g.files.forEach((f, fi) => {
        if (marks[gi]?.[fi]) {
          for (const x of [t.all, t[g.kind]]) {
            x.count += 1 + f.links;
            x.freed += f.size;
          }
        }
      });
    });
    return t;
  }, [result, marks]);
}

async function findDuplicates() {
  const st = useStore.getState();
  const ds = st.settings?.dupes;
  if (!ds || !st.dupes.roots.length) return;
  st.setDupes((d) => ({ ...d, running: true, progress: null, notice: null, focus: null, result: null, marks: [] }));
  try {
    const r = await api.dupesFind({
      roots: st.dupes.roots,
      recursive: true,
      include_hidden: ds.include_hidden,
      include_cloud: ds.include_cloud,
      min_size: ds.min_size,
      kinds: ds.kinds,
      exclude: ds.exclude ?? [],
      use_cache: true,
      find_formats: ds.find_formats ?? true,
    });
    useStore.getState().setDupes((d) => ({
      ...d,
      running: false,
      result: r,
      marks: r.marks,
      focus: r.groups.length ? { g: 0, f: 0 } : null,
      notice: r.groups.length ? null : `Одинаковых файлов не нашлось. Проверено ${files(r.scanned)}.`,
    }));
    for (const [path, error] of r.errors) useStore.getState().addLog("Warn", `${basename(path)}: ${error}`, path);
  } catch (e) {
    const msg = String(e);
    useStore.getState().setDupes((d) => ({
      ...d,
      running: false,
      notice: msg.includes("отменено") ? "Поиск остановлен." : `Поиск не удался: ${msg}`,
    }));
  }
}

/** Выполняет действие над отмеченными файлами и убирает их из результата.
 *  `only` — только группы этого типа: в простом режиме точные копии и одно фото
 *  в разных форматах убираются разными кнопками. */
async function applyMarks(action: DupAction, only?: GroupKind) {
  const st = useStore.getState();
  const groups = st.dupes.result?.groups ?? [];
  const marks = st.dupes.marks.map((m, gi) => (only && groups[gi]?.kind !== only ? m.map(() => false) : m));
  let freed = 0;
  groups.forEach((g, gi) =>
    g.files.forEach((f, fi) => {
      if (marks[gi]?.[fi]) freed += f.size;
    }),
  );
  try {
    const results = await api.dupesApply(marks, action);
    const ok = new Set(results.filter((r) => !r.error).map((r) => r.path));
    const failed = results.filter((r) => r.error);
    for (const r of failed) useStore.getState().addLog("Error", `${basename(r.path)}: ${r.error}`, r.path);
    useStore.getState().setDupes((d) => {
      if (!d.result) return d;
      const groups: typeof d.result.groups = [];
      const nextMarks: boolean[][] = [];
      d.result.groups.forEach((g, gi) => {
        const keep = g.files.map((f, fi) => ({ f, m: d.marks[gi]?.[fi] ?? false })).filter(({ f }) => !ok.has(f.path));
        if (keep.length > 1) {
          groups.push({ ...g, files: keep.map((k) => k.f) });
          nextMarks.push(keep.map((k) => k.m));
        }
      });
      const done =
        action === "Trash"
          ? `Убрано в корзину: ${files(ok.size)}, освобождено ${bytes(freed)}. Вернуть их можно из корзины.`
          : action === "Delete"
            ? `Удалено: ${files(ok.size)}, освобождено ${bytes(freed)}.`
            : `Перемещено: ${files(ok.size)}.`;
      return {
        ...d,
        result: { ...d.result, groups },
        marks: nextMarks,
        focus: groups.length ? { g: 0, f: 0 } : null,
        notice: failed.length ? `${done} Не получилось: ${files(failed.length)} — подробности во вкладке «Подробности».` : done,
      };
    });
  } catch (e) {
    useStore.getState().setDupes((d) => ({ ...d, notice: `Не получилось: ${e}` }));
  }
}

export function DupesView() {
  const setDupes = useStore((s) => s.setDupes);
  const [review, setReview] = useState(false);

  useEffect(() => {
    const un = listen<DupesProgress>("dupes:progress", (e) => setDupes((d) => ({ ...d, progress: e.payload })));
    return () => void un.then((f) => f());
  }, [setDupes]);

  return review ? <Review onBack={() => setReview(false)} /> : <Simple onReview={() => setReview(true)} />;
}

/** Основной режим: три шага одной колонкой (ТЗ 8.5). */
function Simple({ onReview }: { onReview: () => void }) {
  const dupes = useStore((s) => s.dupes);
  const setDupes = useStore((s) => s.setDupes);
  const settings = useStore((s) => s.settings);
  const updateSettings = useStore((s) => s.updateSettings);
  const totals = useTotals();
  const [places, setPlaces] = useState<{ label: string; path: string }[]>([]);

  useEffect(() => {
    const known: [string, () => Promise<string>][] = [
      ["Изображения", pictureDir],
      ["Загрузки", downloadDir],
      ["Рабочий стол", desktopDir],
    ];
    void Promise.all(
      known.map(async ([label, fn]) => {
        try {
          return { label, path: await fn() };
        } catch {
          return null;
        }
      }),
    ).then((list) => setPlaces(list.filter((x): x is { label: string; path: string } => x !== null)));
  }, []);

  if (!settings) return null;
  const ds = settings.dupes;
  const result = dupes.result;
  const groups = result?.groups.length ?? 0;
  const prog = progressText(dupes.progress);
  const addRoots = (list: string[]) => setDupes((d) => ({ ...d, roots: [...new Set([...d.roots, ...list])] }));
  const keepLabel = RULES.find((r) => r.value === ruleKey(ds.keep))!.label.toLowerCase();
  const keepFormat = ds.keep_format ?? "Jpeg";

  const remark = async (rule: KeepRule, format: PhotoKind) => {
    if (useStore.getState().dupes.result) {
      const next = await api.dupesAuto(rule, format);
      setDupes((d) => ({ ...d, marks: next }));
    }
  };
  const setRule = async (key: RuleKey) => {
    const rule: KeepRule = key === "PriorityFolders" ? { PriorityFolders: dupes.roots } : key;
    updateSettings((s) => ({ ...s, dupes: { ...s.dupes, keep: rule } }));
    await remark(rule, keepFormat);
  };
  const setKeepFormat = async (format: PhotoKind) => {
    updateSettings((s) => ({ ...s, dupes: { ...s.dupes, keep_format: format } }));
    await remark(ds.keep, format);
  };

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto max-w-[760px] space-y-8 px-6 py-8">
        <section className="space-y-4">
          <Step n={1} title="Где искать одинаковые файлы" />
          <div className="space-y-2">
            {dupes.roots.map((r) => (
              <div key={r} className="flex items-center gap-3 rounded-lg border border-line bg-surface py-1 pr-1 pl-4">
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[16px] font-medium">{basename(r) || r}</span>
                  <span className="block truncate text-[14px] text-muted">{dirname(r)}</span>
                </span>
                <IconButton
                  label="Убрать"
                  disabled={dupes.running}
                  onClick={() => setDupes((d) => ({ ...d, roots: d.roots.filter((x) => x !== r) }))}
                >
                  <X className="size-5" />
                </IconButton>
              </div>
            ))}
          </div>
          <div className="flex flex-wrap gap-3">
            <Button
              disabled={dupes.running}
              onClick={async () => {
                const picked = await open({ directory: true, multiple: true, title: "Где искать одинаковые файлы" });
                addRoots(picked == null ? [] : Array.isArray(picked) ? picked : [picked]);
              }}
            >
              <FolderPlus className="size-5" />
              Выбрать папку
            </Button>
            {places
              .filter((p) => !dupes.roots.includes(p.path))
              .map((p) => (
                <Button key={p.path} variant="ghost" disabled={dupes.running} onClick={() => addRoots([p.path])}>
                  + {p.label}
                </Button>
              ))}
          </div>
        </section>

        <section className="space-y-4">
          <Step n={2} title="Найти" />
          {dupes.running ? (
            <div className="space-y-3">
              <p className="text-[16px]">{prog.text}</p>
              <ProgressBar value={prog.fraction ?? 0} />
              <Button onClick={() => void api.dupesCancel()}>
                <Square className="size-4" />
                Остановить
              </Button>
            </div>
          ) : (
            <>
              <Button variant="primary" size="lg" disabled={!dupes.roots.length} onClick={() => void findDuplicates()}>
                Найти одинаковые файлы
              </Button>
              {!dupes.roots.length && <p className="text-[16px] text-muted">Сначала выберите, где искать, — шаг 1.</p>}
            </>
          )}
        </section>

        {(result || dupes.notice) && !dupes.running && (
          <section className="space-y-4">
            <Step n={3} title="Результат" />
            {dupes.notice && (
              <p className="rounded-lg bg-accent-soft px-4 py-3 text-[16px] leading-relaxed">{dupes.notice}</p>
            )}
            {groups > 0 && (
              <div className="space-y-5">
                {totals.Exact.groups > 0 && (
                  <div className="space-y-4">
                    <p className="text-[18px] leading-relaxed">
                      Нашлось {totals.Exact.count}{" "}
                      {plural(totals.Exact.count, "лишняя копия", "лишние копии", "лишних копий")} одинаковых файлов.
                      Если их убрать, освободится <span className="font-semibold">{bytes(totals.Exact.freed)}</span>. В
                      каждой группе останется {keepLabel}.
                    </p>
                    <Button
                      variant="primary"
                      size="lg"
                      disabled={!totals.Exact.count}
                      onClick={() => void applyMarks("Trash", "Exact")}
                    >
                      Убрать лишние копии в корзину
                    </Button>
                  </div>
                )}
                {totals.Formats.groups > 0 && (
                  <div className="space-y-4 rounded-lg border border-line bg-surface px-5 py-4">
                    <p className="text-[18px] leading-relaxed">
                      {totals.Exact.groups > 0 ? "Ещё " : ""}
                      {totals.Formats.groups}{" "}
                      {plural(totals.Formats.groups, "фото сохранено", "фото сохранены", "фото сохранены")} в нескольких
                      форматах: например, снимок с телефона и его копия JPEG. Это не одинаковые файлы, а одно и то же
                      фото. Если оставить у каждого только {formatLabel(keepFormat)}, освободится{" "}
                      <span className="font-semibold">{bytes(totals.Formats.freed)}</span>.
                    </p>
                    <Button
                      variant={totals.Exact.groups > 0 ? "secondary" : "primary"}
                      size="lg"
                      disabled={!totals.Formats.count}
                      onClick={() => void applyMarks("Trash", "Formats")}
                    >
                      Оставить только {formatLabel(keepFormat)}, остальное в корзину
                    </Button>
                    <p className="text-[14px] text-muted">
                      Какой формат оставлять, можно выбрать в расширенных настройках ниже.
                    </p>
                  </div>
                )}
                <div className="flex flex-wrap items-center gap-3">
                  <Button size="lg" onClick={onReview}>
                    Посмотреть и выбрать вручную
                  </Button>
                  <p className="text-[14px] text-muted">Из корзины файлы можно вернуть, если передумаете.</p>
                </div>
              </div>
            )}
          </section>
        )}

        <Advanced
          id="dupes"
          inset="px-0"
          className="border-t border-line"
          changed={[
            ruleKey(ds.keep) !== "Oldest" && "что оставлять",
            ds.find_formats === false && "без поиска одного фото в разных форматах",
            keepFormat !== "Jpeg" && "какой формат оставлять",
            kindKey(ds.kinds) !== "All" && "какие файлы",
            ds.min_size !== 1024 && "минимальный размер",
            (ds.exclude ?? []).length > 0 && "исключения",
            ds.include_hidden && "скрытые файлы",
            ds.include_cloud && "облачные файлы",
          ].filter((x): x is string => Boolean(x))}
        >
          <div className="space-y-5 pb-4">
            <Field label="Что оставлять в каждой группе">
              <Select<RuleKey> value={ruleKey(ds.keep)} onChange={(k) => void setRule(k)} options={RULES} />
            </Field>
            <Switch
              checked={ds.find_formats ?? true}
              onChange={(v) => updateSettings((s) => ({ ...s, dupes: { ...s.dupes, find_formats: v } }))}
              label="Искать одно фото, сохранённое в разных форматах"
              hint="Например, снимок с iPhone и его копию JPEG после преобразования"
            />
            {(ds.find_formats ?? true) && (
              <Field label="Если одно фото сохранено в разных форматах, оставлять">
                <Select<PhotoKind> value={keepFormat} onChange={(k) => void setKeepFormat(k)} options={FORMATS} />
              </Field>
            )}
            <Field label="Какие файлы сравнивать">
              <Select
                value={kindKey(ds.kinds)}
                onChange={(v) =>
                  updateSettings((s) => ({
                    ...s,
                    dupes: { ...s.dupes, kinds: v === "Extensions" ? { Extensions: ["jpg", "heic"] } : (v as KindFilter) },
                  }))
                }
                options={[
                  { value: "All", label: "Все файлы" },
                  { value: "Images", label: "Только фото" },
                  { value: "Videos", label: "Только видео" },
                  { value: "Extensions", label: "Свой список расширений" },
                ]}
              />
            </Field>
            {typeof ds.kinds === "object" && (
              <Field label="Расширения через запятую" hint="Например: jpg, heic, mov">
                <TextInput
                  value={ds.kinds.Extensions.join(", ")}
                  onCommit={(text) =>
                    updateSettings((s) => ({ ...s, dupes: { ...s.dupes, kinds: { Extensions: splitList(text) } } }))
                  }
                />
              </Field>
            )}
            <Field
              label="Не искать в папках"
              hint="Через запятую. Папка пропускается, если её путь содержит одно из слов, например: Backup, .thumbnails"
            >
              <TextInput
                value={(ds.exclude ?? []).join(", ")}
                onCommit={(text) => updateSettings((s) => ({ ...s, dupes: { ...s.dupes, exclude: splitList(text) } }))}
              />
            </Field>
            <Field label="Не меньше" hint="Мелкие файлы обычно служебные, их можно не проверять.">
              <Select
                value={String(ds.min_size)}
                onChange={(v) => updateSettings((s) => ({ ...s, dupes: { ...s.dupes, min_size: Number(v) } }))}
                options={[
                  { value: "1", label: "Любого размера" },
                  { value: "1024", label: "1 КБ" },
                  { value: "102400", label: "100 КБ" },
                  { value: "1048576", label: "1 МБ" },
                  { value: "10485760", label: "10 МБ" },
                ]}
              />
            </Field>
            <Switch
              checked={ds.include_hidden}
              onChange={(v) => updateSettings((s) => ({ ...s, dupes: { ...s.dupes, include_hidden: v } }))}
              label="Проверять скрытые файлы и папки"
            />
            <Switch
              checked={ds.include_cloud}
              onChange={(v) => updateSettings((s) => ({ ...s, dupes: { ...s.dupes, include_cloud: v } }))}
              label="Проверять облачные файлы без копии на диске"
              hint="Поиск скачает их из OneDrive или другого облака"
            />
          </div>
        </Advanced>
      </div>
    </div>
  );
}

/** Поле, которое сохраняет значение по уходу фокуса: список не дёргается на каждой букве. */
function TextInput({ value, onCommit }: { value: string; onCommit: (v: string) => void }) {
  const [text, setText] = useState(value);
  useEffect(() => setText(value), [value]);
  return (
    <input
      value={text}
      onChange={(e) => setText(e.target.value)}
      onBlur={() => text !== value && onCommit(text)}
      onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
      spellCheck={false}
      className="h-11 w-full rounded-lg border border-line-strong bg-surface px-3 text-[16px]"
    />
  );
}

function ThumbImg({ path, size, className }: { path: string; size?: number; className: string }) {
  const src = useThumb(path, size);
  if (src === false) return <div className="py-16 text-center text-[16px] text-muted">Превью недоступно</div>;
  if (!src) return <div className="h-48" />;
  return <img src={src} alt="" className={className} />;
}

/** Ручной разбор: группы, отметки, предпросмотр. */
function Review({ onBack }: { onBack: () => void }) {
  const dupes = useStore((s) => s.dupes);
  const setDupes = useStore((s) => s.setDupes);
  const addLog = useStore((s) => s.addLog);
  const totals = useTotals();
  const [action, setAction] = useState<ActionKey>("Trash");
  const [confirm, setConfirm] = useState(false);
  const [preview, setPreview] = useState(false);
  const [compare, setCompare] = useState(false);
  const listRef = useRef<HTMLDivElement>(null);
  const result = dupes.result;
  const marks = dupes.marks;

  const flat = useMemo<FlatRow[]>(() => {
    const rows: FlatRow[] = [];
    result?.groups.forEach((g, gi) => {
      rows.push({ type: "group", g: gi });
      g.files.forEach((_, fi) => rows.push({ type: "file", g: gi, f: fi }));
    });
    return rows;
  }, [result]);

  const virtualizer = useVirtualizer({
    count: flat.length,
    getScrollElement: () => listRef.current,
    estimateSize: (i) => (flat[i].type === "group" ? 44 : 68),
    overscan: 12,
  });

  const toggle = (g: number, f: number) => {
    const group = marks[g];
    if (!group) return;
    const next = !group[f];
    if (next && group.filter((m, i) => !m && i !== f).length === 0) {
      setDupes((d) => ({ ...d, notice: "В каждой группе должен остаться хотя бы один файл." }));
      return;
    }
    setDupes((d) => ({
      ...d,
      notice: null,
      marks: d.marks.map((m, gi) => (gi === g ? m.map((v, fi) => (fi === f ? next : v)) : m)),
    }));
  };

  const apply = async () => {
    setConfirm(false);
    if (action === "MoveTo") {
      const dir = await open({ directory: true, title: "Куда переместить лишние копии" });
      if (typeof dir !== "string") return;
      await applyMarks({ MoveTo: dir });
    } else {
      await applyMarks(action);
    }
  };

  const exportCsv = async () => {
    const path = await save({
      title: "Сохранить список",
      defaultPath: "одинаковые файлы.csv",
      filters: [{ name: "CSV", extensions: ["csv"] }],
    });
    if (path) await api.dupesExportCsv(path, marks).catch((e) => addLog("Error", `Список не сохранён: ${e}`));
  };

  // Клавиши — только дополнение к кнопкам (FR-10.6, ТЗ 8.5).
  const onKeyDown = (e: React.KeyboardEvent) => {
    if (!result || !dupes.focus) return;
    const { g, f } = dupes.focus;
    const move = (focus: { g: number; f: number }) => {
      setDupes((d) => ({ ...d, focus }));
      const idx = flat.findIndex((r) => r.type === "file" && r.g === focus.g && r.f === focus.f);
      if (idx >= 0) virtualizer.scrollToIndex(idx, { align: "auto" });
      e.preventDefault();
    };
    if (e.key === "ArrowDown" && (e.ctrlKey || e.metaKey)) move({ g: Math.min(g + 1, result.groups.length - 1), f: 0 });
    else if (e.key === "ArrowUp" && (e.ctrlKey || e.metaKey)) move({ g: Math.max(g - 1, 0), f: 0 });
    else if (e.key === "ArrowDown") {
      if (f + 1 < result.groups[g].files.length) move({ g, f: f + 1 });
      else if (g + 1 < result.groups.length) move({ g: g + 1, f: 0 });
    } else if (e.key === "ArrowUp") {
      if (f > 0) move({ g, f: f - 1 });
      else if (g > 0) move({ g: g - 1, f: result.groups[g - 1].files.length - 1 });
    } else if (e.key === "Delete" || e.key === "Backspace") {
      toggle(g, f);
      e.preventDefault();
    } else if (e.key === " ") {
      setPreview((v) => !v);
      e.preventDefault();
    }
  };

  const focused = result && dupes.focus ? result.groups[dupes.focus.g]?.files[dupes.focus.f] : undefined;
  const actionLabel = action === "Trash" ? "Убрать в корзину" : action === "MoveTo" ? "Переместить…" : "Удалить навсегда";

  return (
    <div className="flex min-h-0 flex-1">
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="flex shrink-0 items-center gap-3 border-b border-line px-4 py-2">
          <Button variant="ghost" onClick={onBack}>
            <ArrowLeft className="size-5" />
            Вернуться
          </Button>
          <span className="text-[16px] text-muted">Отмеченные крестиком копии будут убраны. Один файл в группе остаётся.</span>
          <Button variant="ghost" className="ml-auto" onClick={() => void exportCsv()}>
            <FileDown className="size-5" />
            Сохранить список
          </Button>
        </div>
        {dupes.notice && <div className="border-b border-line bg-warn-soft px-5 py-3 text-[16px]">{dupes.notice}</div>}
        {!result || result.groups.length === 0 ? (
          <div className="flex flex-1 items-center justify-center text-[16px] text-muted">Одинаковых файлов больше нет.</div>
        ) : (
          <div ref={listRef} className="min-h-0 flex-1 overflow-y-auto" tabIndex={0} onKeyDown={onKeyDown}>
            <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
              {virtualizer.getVirtualItems().map((v) => {
                const row = flat[v.index];
                const style = {
                  position: "absolute" as const,
                  top: 0,
                  left: 0,
                  right: 0,
                  height: v.size,
                  transform: `translateY(${v.start}px)`,
                };
                const group = result.groups[row.g];
                if (row.type === "group") {
                  return (
                    <div key={`g${row.g}`} style={style} className="flex items-end px-5 pb-2 text-[16px] font-semibold">
                      {group.kind === "Formats" ? (
                        <>Одно фото в {group.files.length} {plural(group.files.length, "формате", "форматах", "форматах")}</>
                      ) : (
                        <>
                          {group.files.length}{" "}
                          {plural(group.files.length, "одинаковый файл", "одинаковых файла", "одинаковых файлов")} по{" "}
                          {bytes(group.size)}
                        </>
                      )}
                    </div>
                  );
                }
                const file = group.files[row.f];
                const marked = marks[row.g]?.[row.f] ?? false;
                const isFocus = dupes.focus?.g === row.g && dupes.focus?.f === row.f;
                return (
                  <div
                    key={`f${row.g}-${row.f}`}
                    style={style}
                    onClick={() => setDupes((d) => ({ ...d, focus: { g: row.g, f: row.f } }))}
                    className={clsx(
                      "flex items-center gap-3 border-l-4 pr-5 pl-3",
                      isFocus ? "border-accent bg-accent-soft" : "border-transparent hover:bg-sunken/70",
                    )}
                  >
                    <Checkbox checked={marked} onChange={() => toggle(row.g, row.f)} label="Убрать эту копию" />
                    <Thumb path={file.path} kind={file.kind} size={52} />
                    <div className="min-w-0 flex-1">
                      <div className={clsx("truncate text-[16px]", marked && "text-muted line-through decoration-err/60")}>
                        {basename(file.path)}
                      </div>
                      <div className="truncate text-[14px] text-muted">
                        {dirname(file.path)}
                        {file.links > 0 &&
                          ` (ещё ${file.links} ${plural(file.links, "ссылка", "ссылки", "ссылок")} на этот же файл)`}
                      </div>
                    </div>
                    <span className="w-44 shrink-0 text-right text-[14px] text-muted tabular-nums">
                      {group.kind === "Formats" ? `${file.format ?? ""}, ${bytes(file.size)}` : date(file.mtimeMs)}
                    </span>
                    <span className={clsx("w-24 shrink-0 text-right text-[16px] font-medium", marked ? "text-err" : "text-ok")}>
                      {marked ? "убрать" : "оставить"}
                    </span>
                  </div>
                );
              })}
            </div>
          </div>
        )}
        {result && result.groups.length > 0 && (
          <div className="flex shrink-0 flex-wrap items-center gap-3 border-t border-line bg-surface px-5 py-3">
            <div className="min-w-0 flex-1 text-[16px]">
              {totals.all.count > 0 ? (
                <>
                  Будет убрано {files(totals.all.count)}, освободится <span className="font-semibold">{bytes(totals.all.freed)}</span>
                </>
              ) : (
                <span className="text-muted">Отметьте крестиком копии, которые не нужны</span>
              )}
            </div>
            <div className="w-64">
              <Select<ActionKey>
                ariaLabel="Что сделать с отмеченными"
                value={action}
                onChange={setAction}
                options={[
                  { value: "Trash", label: "Убрать в корзину", hint: "Можно вернуть" },
                  { value: "MoveTo", label: "Переместить в папку…" },
                  { value: "Delete", label: "Удалить навсегда", hint: "Без корзины, вернуть нельзя" },
                ]}
              />
            </div>
            <Button
              variant={action === "Delete" ? "danger" : "primary"}
              disabled={totals.all.count === 0}
              onClick={() => (action === "Delete" ? setConfirm(true) : void apply())}
            >
              {actionLabel}
            </Button>
          </div>
        )}
      </div>

      <aside className="flex w-[340px] shrink-0 flex-col border-l border-line bg-surface">
        {focused ? (
          <div className="min-h-0 flex-1 space-y-4 overflow-y-auto px-5 py-5">
            <button
              className="block w-full overflow-hidden rounded-lg bg-sunken"
              onClick={() => setPreview(true)}
              aria-label="Показать крупно"
            >
              <ThumbImg path={focused.path} className="mx-auto max-h-72 object-contain" />
            </button>
            <div className="space-y-1">
              <div className="text-[16px] font-medium break-all">{basename(focused.path)}</div>
              <div className="text-[14px] break-all text-muted selectable">{dirname(focused.path)}</div>
              <div className="text-[14px] text-muted tabular-nums">
                {bytes(focused.size)}, изменён {date(focused.mtimeMs)}
              </div>
            </div>
            <div className="flex flex-wrap gap-2">
              <Button onClick={() => setPreview(true)}>Показать крупно</Button>
              <Button onClick={() => setCompare(true)}>Сравнить с копией</Button>
              <Button onClick={() => void openPath(focused.path)}>Открыть</Button>
              <Button variant="ghost" onClick={() => void revealItemInDir(focused.path)}>
                <FolderOpen className="size-5" />
                Показать в папке
              </Button>
            </div>
          </div>
        ) : (
          <div className="px-5 py-5 text-[16px] text-muted">Нажмите на файл в списке, чтобы увидеть его здесь.</div>
        )}
      </aside>

      <Dialog
        open={confirm}
        onOpenChange={setConfirm}
        title="Удалить навсегда?"
        footer={
          <>
            <Button variant="primary" onClick={() => setConfirm(false)}>
              Не удалять
            </Button>
            <Button variant="danger" onClick={() => void apply()}>
              Удалить {files(totals.all.count)}
            </Button>
          </>
        }
      >
        <p className="leading-relaxed">
          {files(totals.all.count)} будут удалены без корзины, вернуть их будет нельзя. В каждой группе останется по одному
          файлу. Если сомневаетесь, выберите «Убрать в корзину».
        </p>
      </Dialog>

      <CompareDialog
        open={compare}
        onClose={() => setCompare(false)}
        group={dupes.focus ? result?.groups[dupes.focus.g] : undefined}
        focus={dupes.focus?.f ?? 0}
        marks={dupes.focus ? (marks[dupes.focus.g] ?? []) : []}
        onToggle={(f) => dupes.focus && toggle(dupes.focus.g, f)}
      />

      <Dialog open={preview && !!focused} onOpenChange={setPreview} title={focused ? basename(focused.path) : ""} width={900}>
        {focused && <ThumbImg path={focused.path} size={1400} className="mx-auto max-h-[70vh] object-contain" />}
      </Dialog>
    </div>
  );
}
