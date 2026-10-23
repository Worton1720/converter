import { open } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import { FilePlus2, FolderOpen, FolderPlus, LayoutGrid, List, Pause, Play, Smartphone, Square } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { Button, Dialog, ProgressBar, Step } from "../../components/ui";
import { api } from "../../lib/api";
import { basename, bytes, dirname, etaWords, files, mediaCount, plainError } from "../../lib/format";
import { useStore } from "../../lib/store";
import type { ConvertOptions, Interrupted, SpaceCheck, SpaceEstimate } from "../../lib/types";
import { FileGrid } from "./FileGrid";
import { PhoneWizard } from "./PhoneWizard";
import { FileTable } from "./FileTable";
import { OptionsPanel } from "./OptionsPanel";

export async function addPaths(paths: string[]) {
  if (!paths.length) return;
  try {
    const added = await api.addPaths(paths);
    if (added === 0) useStore.getState().addLog("Info", "Новых фото и видео не найдено.");
  } catch (e) {
    useStore.getState().addLog("Error", `Не удалось добавить файлы: ${e}`);
  }
}

export async function pickAndAdd(directory: boolean) {
  const picked = await open({
    directory,
    multiple: true,
    title: directory ? "Выберите папку с фото и видео" : "Выберите фото и видео",
    filters: directory
      ? undefined
      : [
          {
            name: "Фото и видео",
            extensions: ["heic", "heif", "avif", "jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "mov", "mp4", "m4v", "mkv", "webm", "avi", "3gp", "hevc", "mts", "m2ts"],
          },
        ],
  });
  const list = picked == null ? [] : Array.isArray(picked) ? picked : [picked];
  await addPaths(list);
}

function PickButtons({ big }: { big?: boolean }) {
  const [phone, setPhone] = useState(false);
  return (
    <div className="flex flex-wrap gap-3">
      <Button variant={big ? "primary" : "secondary"} size={big ? "lg" : "md"} onClick={() => setPhone(true)}>
        <Smartphone className="size-5" />
        Перенести с телефона
      </Button>
      <PhoneWizard open={phone} onClose={() => setPhone(false)} />
      <Button size={big ? "lg" : "md"} onClick={() => void pickAndAdd(true)}>
        <FolderPlus className="size-5" />
        Выбрать папку
      </Button>
      <Button size={big ? "lg" : "md"} onClick={() => void pickAndAdd(false)}>
        <FilePlus2 className="size-5" />
        Выбрать отдельные файлы
      </Button>
    </div>
  );
}

function EmptyState() {
  return (
    <div className="flex flex-1 items-center justify-center p-10">
      <div className="max-w-xl space-y-5 text-center">
        <p className="text-[20px] leading-snug font-medium">Выберите папку, где лежат фото и видео с телефона</p>
        <p className="text-[16px] leading-relaxed text-muted">
          Программа найдёт в ней все фото и видео, в том числе во вложенных папках. Можно выбрать и отдельные файлы.
        </p>
        <div className="flex justify-center">
          <PickButtons big />
        </div>
        <p className="text-[14px] text-muted">Файлы можно также перетащить в это окно мышью.</p>
      </div>
    </div>
  );
}

/** После сбоя: «В прошлый раз программа закрылась посреди работы» — доделать одной кнопкой.
 *  Уже готовые результаты пропускаются, а параметры того запуска не становятся настройками. */
function ResumeBanner() {
  const [info, setInfo] = useState<Interrupted | null>(null);
  const runActive = useStore((s) => s.run.active);
  const startRun = useStore((s) => s.startRun);
  const addLog = useStore((s) => s.addLog);
  useEffect(() => {
    api.historyInterrupted().then(setInfo, () => setInfo(null));
  }, []);
  if (!info || runActive) return null;

  const resume = async () => {
    setInfo(null);
    try {
      const options = await api.historyResume(info.id);
      await api.addPaths(info.pending);
      startRun(info.pending);
      await api.startConvert(options, info.pending, false);
    } catch (e) {
      addLog("Error", `Не удалось доделать: ${e}`);
      useStore.setState((s) => ({ run: { ...s.run, active: false } }));
    }
  };
  return (
    <div className="flex flex-wrap items-center gap-3 border-b border-line bg-warn-soft px-6 py-3">
      <p className="min-w-0 flex-1 text-[16px] leading-relaxed">
        В прошлый раз программа закрылась посреди работы. Готово {files(info.done)}, осталось{" "}
        {files(info.pending.length)}.
      </p>
      <Button variant="primary" onClick={() => void resume()}>
        Доделать
      </Button>
      <Button variant="ghost" onClick={() => void api.historyDismiss(info.id).then(() => setInfo(null))}>
        Не надо
      </Button>
    </div>
  );
}

export function ConvertView() {
  const order = useStore((s) => s.order);
  const rows = useStore((s) => s.rows);
  const clearItems = useStore((s) => s.clearItems);
  const runActive = useStore((s) => s.run.active);
  const [asList, setAsList] = useState(false);

  const counts = useMemo(() => {
    let images = 0;
    let videos = 0;
    for (const p of order) {
      if (rows[p]?.item.kind === "image") images++;
      else if (rows[p]) videos++;
    }
    return { images, videos };
  }, [order, rows]);

  return (
    <div className="flex min-h-0 flex-1">
      <section className="flex min-w-0 flex-1 flex-col">
        <ResumeBanner />
        <div className="space-y-4 border-b border-line px-6 py-5">
          <Step n={1} title="Выберите фото и видео">
            {order.length > 0 && (
              <span className="text-[16px] text-muted">выбрано {mediaCount(counts.images, counts.videos)}</span>
            )}
          </Step>
          {order.length > 0 && (
            <div className="flex flex-wrap items-center gap-3">
              <PickButtons />
              <div className="ml-auto flex gap-2">
                <Button variant="ghost" onClick={() => setAsList(!asList)}>
                  {asList ? <LayoutGrid className="size-5" /> : <List className="size-5" />}
                  {asList ? "Показать картинками" : "Показать списком"}
                </Button>
                <Button variant="ghost" disabled={runActive} onClick={clearItems}>
                  Убрать все из списка
                </Button>
              </div>
            </div>
          )}
        </div>
        {order.length === 0 ? <EmptyState /> : asList ? <FileTable paths={order} /> : <FileGrid paths={order} />}
      </section>

      <aside className="flex w-[400px] shrink-0 flex-col border-l border-line bg-surface">
        <OptionsPanel />
        <RunPanel />
      </aside>
    </div>
  );
}

/** Шаг 3: запуск, прогресс человеческим языком и понятный итог. */
/** Одна фраза о месте на диске перед стартом (план после 0.2.0, п. 2). Файлы в корзине
 *  место не освобождают, пока её не очистить, — это пожилому пользователю не очевидно. */
function spaceWords(e: SpaceEstimate, sources: ConvertOptions["sources"]): string | null {
  if (e.needed < 1024 * 1024) return null;
  const grows = e.needed > e.sources;
  const base = `Результат займёт около ${bytes(e.needed)}${
    grows ? ` — больше, чем сейчас занимают исходные файлы (${bytes(e.sources)})` : ""
  }.`;
  const free = ` На диске свободно ${bytes(e.free)}.`;
  if (sources === "Keep") return base + free;
  if (sources === "Trash")
    return `${base}${free} Исходные файлы уйдут в корзину и продолжат занимать место, пока её не очистить.`;
  return `${base} Исходные файлы (${bytes(e.sources)}) будут удалены.${free}`;
}

function RunPanel() {
  const order = useStore((s) => s.order);
  const rows = useStore((s) => s.rows);
  const run = useStore((s) => s.run);
  const startRun = useStore((s) => s.startRun);
  const setPaused = useStore((s) => s.setPaused);
  const setDupes = useStore((s) => s.setDupes);
  const setTab = useStore((s) => s.setTab);
  const addLog = useStore((s) => s.addLog);
  const output = useStore((s) => s.settings?.convert.output);
  const ffmpegMissing = useStore((s) => s.info !== null && !s.info.ffmpeg);
  const [space, setSpace] = useState<{ paths: string[]; checks: SpaceCheck[] } | null>(null);
  const [details, setDetails] = useState(false);
  const options = useStore((s) => s.settings?.convert);
  const [estimate, setEstimate] = useState<SpaceEstimate | null>(null);

  // Запускаем всё, что ещё не готово.
  const targets = useMemo(
    () => order.filter((p) => rows[p] && rows[p].job.status !== "done" && rows[p].job.status !== "running"),
    [order, rows],
  );
  const targetCounts = useMemo(() => {
    let images = 0;
    let videos = 0;
    for (const p of targets) {
      if (rows[p].item.kind === "image") images++;
      else videos++;
    }
    return { images, videos };
  }, [targets, rows]);
  // Оценка места — с задержкой: список и настройки меняются пачками.
  useEffect(() => {
    if (!options || !targets.length || run.active) {
      setEstimate(null);
      return;
    }
    const t = setTimeout(() => {
      api.spaceEstimate(options, targets).then(setEstimate, () => setEstimate(null));
    }, 400);
    return () => clearTimeout(t);
  }, [options, targets, run.active]);

  const failed = useMemo(() => run.paths.filter((p) => rows[p]?.job.status === "failed"), [run.paths, rows]);
  const outputDirs = useMemo(() => {
    const dirs = new Set<string>();
    for (const p of run.paths) {
      const out = rows[p]?.job.output;
      if (out) dirs.add(dirname(out));
    }
    return [...dirs];
  }, [run.paths, rows]);

  const progress = useMemo(() => {
    let doneFraction = 0;
    for (const p of run.paths) {
      const j = rows[p]?.job;
      if (!j) continue;
      if (j.status === "running") doneFraction += j.progress;
      else if (j.status !== "waiting") doneFraction += 1;
    }
    const total = run.paths.length;
    const elapsed = (Date.now() - run.startedAt) / 1000;
    const remaining = doneFraction > 0.05 ? (elapsed * (total - doneFraction)) / doneFraction : NaN;
    return { fraction: total ? doneFraction / total : 0, total, remaining };
  }, [rows, run]);

  const launch = async (paths: string[], skipSpaceCheck = false) => {
    const options = useStore.getState().settings?.convert;
    if (!options || !paths.length) return;
    if (!skipSpaceCheck) {
      const checks = await api.spaceCheck(options, paths).catch(() => []);
      if (checks.length) {
        setSpace({ paths, checks });
        return;
      }
    }
    startRun(paths);
    try {
      await api.startConvert(options, paths);
    } catch (e) {
      addLog("Error", `Не удалось начать: ${e}`);
      useStore.setState((s) => ({ run: { ...s.run, active: false } }));
    }
  };

  const findDupes = () => {
    setDupes((d) => ({ ...d, roots: [...new Set([...d.roots, ...outputDirs])] }));
    setTab("dupes");
  };

  const where =
    outputDirs.length === 1
      ? `в папку «${basename(outputDirs[0])}»`
      : typeof output === "string"
        ? "рядом с исходными файлами"
        : "в выбранную папку";

  let body;
  if (run.active) {
    body = (
      <div className="space-y-3">
        <p className="text-[20px] font-semibold tabular-nums">
          Сделано {run.finished} из {files(progress.total)}
        </p>
        <ProgressBar value={progress.fraction} paused={run.paused} />
        <p className="text-[16px] text-muted">
          {run.noSpace
            ? "Кончилось место на диске. Освободите место и нажмите «Продолжить»."
            : run.paused
              ? "Пауза. Начатые файлы доделываются, остальные ждут."
              : progress.remaining
                ? `Осталось ${etaWords(progress.remaining)}`
                : "Считаю, сколько осталось…"}
        </p>
        <div className="flex gap-3">
          <Button
            className="flex-1"
            onClick={() => {
              const next = !run.paused;
              setPaused(next);
              void api.pauseConvert(next);
            }}
          >
            {run.paused ? <Play className="size-5" /> : <Pause className="size-5" />}
            {run.paused ? "Продолжить" : "Пауза"}
          </Button>
          <Button className="flex-1" onClick={() => void api.cancelConvert()}>
            <Square className="size-4" />
            Остановить
          </Button>
        </div>
      </div>
    );
  } else if (run.summary && targets.length === 0) {
    const s = run.summary;
    body = (
      <div className="space-y-3">
        <p className="text-[20px] font-semibold text-ok">Готово</p>
        <p className="text-[16px] leading-relaxed">
          {s.done > 0 ? `Сохранено ${files(s.done)} ${where}.` : "Ни один файл не был сохранён."}
          {s.skipped > 0 && ` Пропущено: ${s.skipped}.`}
        </p>
        {outputDirs.length > 0 && (
          <Button variant="primary" size="lg" className="w-full" onClick={() => void openPath(outputDirs[0])}>
            <FolderOpen className="size-5" />
            Открыть папку с результатом
          </Button>
        )}
        <Button className="w-full" onClick={findDupes} disabled={outputDirs.length === 0}>
          Найти одинаковые файлы в этой папке
        </Button>
      </div>
    );
  } else {
    const s = run.summary;
    body = (
      <div className="space-y-3">
        {s && s.failed > 0 && (
          <div className="space-y-2 rounded-lg bg-err-soft px-4 py-3">
            <p className="text-[16px]">
              Не получилось: {files(s.failed)}. Остальные сохранены {where}.
            </p>
            <div className="flex flex-wrap gap-2">
              <Button onClick={() => setDetails(true)}>Подробнее</Button>
              <Button onClick={() => void launch(failed)}>Попробовать ещё раз</Button>
            </div>
          </div>
        )}
        {ffmpegMissing ? (
          <p className="text-[16px] text-err">
            Программе не хватает компонента ffmpeg, без него преобразование невозможно. Откройте «Настройки» →
            «Расширенные настройки» и укажите папку с ffmpeg.
          </p>
        ) : (
          <div className="space-y-1">
            <p className="text-[16px] text-muted">
              {targets.length
                ? `Будет обработано ${mediaCount(targetCounts.images, targetCounts.videos)}.`
                : "Сначала выберите фото и видео — шаг 1."}
            </p>
            {targets.length > 0 && estimate && options && (
              <p className="text-[16px] text-muted">{spaceWords(estimate, options.sources)}</p>
            )}
          </div>
        )}
        <Button
          variant="primary"
          size="lg"
          className="w-full"
          disabled={!targets.length || ffmpegMissing}
          onClick={() => void launch(targets)}
        >
          <Play className="size-5" />
          Начать
        </Button>
      </div>
    );
  }

  return (
    <div className="space-y-4 border-t-2 border-line px-5 py-5">
      <Step n={3} title={run.active ? "Идёт работа" : "Начать"} />
      {body}

      <Dialog
        open={space !== null}
        onOpenChange={(v) => !v && setSpace(null)}
        title="Может не хватить места"
        footer={
          <>
            <Button onClick={() => setSpace(null)}>Не начинать</Button>
            <Button
              variant="primary"
              onClick={() => {
                const paths = space!.paths;
                setSpace(null);
                void launch(paths, true);
              }}
            >
              Всё равно начать
            </Button>
          </>
        }
      >
        <div className="space-y-3 leading-relaxed">
          {space?.checks.map((c) => (
            <p key={c.mount}>
              На диске {c.mount} свободно {bytes(c.free)}, а для результата нужно примерно {bytes(c.needed)}.
            </p>
          ))}
          <p className="text-muted">Если место кончится, работа встанет на паузу. Исходные файлы при этом не пострадают.</p>
        </div>
      </Dialog>

      <Dialog
        open={details}
        onOpenChange={setDetails}
        title="Что не получилось"
        width={600}
        footer={
          <>
            <Button
              onClick={() => {
                setDetails(false);
                setTab("log");
              }}
            >
              Технические подробности
            </Button>
            <Button variant="primary" onClick={() => setDetails(false)}>
              Понятно
            </Button>
          </>
        }
      >
        <ul className="space-y-3">
          {failed.map((p) => (
            <li key={p} className="leading-snug">
              <span className="font-medium break-all">{basename(p)}</span>
              <span className="block text-muted">{plainError(rows[p]?.job.message)}</span>
            </li>
          ))}
        </ul>
      </Dialog>
    </div>
  );
}
