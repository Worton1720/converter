import { listen } from "@tauri-apps/api/event";
import { pictureDir } from "@tauri-apps/api/path";
import { open } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import { FolderOpen, Smartphone } from "lucide-react";
import { useEffect, useState } from "react";
import { Button, Dialog, ProgressBar } from "../../components/ui";
import { api } from "../../lib/api";
import { bytes, files, plural } from "../../lib/format";
import { useStore } from "../../lib/store";
import type { PhonePlan, PhonePlanItem, PhoneReport, PhoneScan, PhoneSource } from "../../lib/types";
import { addPaths } from "./ConvertView";
import { PhoneGallery } from "./PhoneGallery";

type Stage =
  | { kind: "search" }
  | { kind: "none" }
  | { kind: "choose"; sources: PhoneSource[] }
  /** `items` — новое, найденное к этому моменту: галерея наполняется по ходу поиска. */
  | { kind: "planning"; found: number; items: PhonePlanItem[] }
  | { kind: "ready"; plan: PhonePlan }
  | { kind: "copying"; count: number; total: number; files: number; bytes: number }
  | { kind: "done"; report: PhoneReport }
  | { kind: "error"; message: string };

const media = (photos: number, videos: number) =>
  [
    photos ? `${photos} ${plural(photos, "фото", "фото", "фото")}` : "",
    videos ? `${videos} ${plural(videos, "видео", "видео", "видео")}` : "",
  ]
    .filter(Boolean)
    .join(" и ");

/** «Перенести фото с телефона»: найти телефон → куда сохранить → перенести (ТЗ 8.5, пошагово).
 *  С телефона ничего не удаляется; уже перенесённое пропускается. */
export function PhoneWizard({ open: isOpen, onClose }: { open: boolean; onClose: () => void }) {
  const [stage, setStage] = useState<Stage>({ kind: "search" });
  const [device, setDevice] = useState<{ index: number; name: string } | null>(null);
  const [dest, setDest] = useState<string | null>(null);
  /** Выбранное в галерее (номера в плане); по умолчанию — всё новое. */
  const [picked, setPicked] = useState<Set<number>>(new Set());
  const addLog = useStore((s) => s.addLog);

  const plan = async (index: number, folder: string) => {
    // Новое выбрано сразу; снятое человеком во время поиска остаётся снятым.
    let items: PhonePlanItem[] = [];
    let finished = false;
    setPicked(new Set());
    setStage({ kind: "planning", found: 0, items });
    const un = await listen<PhoneScan>("phone:scan", (e) => {
      if (finished) return;
      const from = items.length;
      items = [...items, ...e.payload.items];
      if (e.payload.items.length)
        setPicked((prev) => new Set([...prev, ...e.payload.items.map((_, i) => from + i)]));
      setStage({ kind: "planning", found: e.payload.seen, items });
    });
    try {
      const p = await api.phonePlan(index, folder);
      finished = true;
      const rest = p.items.map((_, i) => i).filter((i) => i >= items.length);
      setPicked((prev) => new Set([...prev, ...rest]));
      setStage({ kind: "ready", plan: p });
    } catch (e) {
      finished = true;
      setStage({ kind: "error", message: String(e) });
    } finally {
      un();
    }
  };

  const search = async () => {
    setStage({ kind: "search" });
    setDevice(null);
    try {
      const sources = await api.phoneSources();
      const folder = dest ?? `${await pictureDir()}/С телефона`;
      setDest(folder);
      if (sources.length === 0) setStage({ kind: "none" });
      else if (sources.length === 1) {
        setDevice({ index: 0, name: sources[0].name });
        await plan(0, folder);
      } else setStage({ kind: "choose", sources });
    } catch (e) {
      setStage({ kind: "error", message: String(e) });
    }
  };

  useEffect(() => {
    if (isOpen) void search();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isOpen]);

  const changeDest = async () => {
    const dir = await open({ directory: true, title: "Куда сохранить фото с телефона" });
    if (typeof dir !== "string") return;
    setDest(dir);
    if (device) await plan(device.index, dir);
  };

  const start = async (p: PhonePlan) => {
    const chosen = [...picked].sort((a, b) => a - b);
    const total = chosen.reduce((sum, i) => sum + p.items[i].size, 0);
    setStage({ kind: "copying", count: chosen.length, total, files: 0, bytes: 0 });
    const un = await listen<{ files: number; bytes: number }>("phone:progress", (e) =>
      setStage({ kind: "copying", count: chosen.length, total, ...e.payload }),
    );
    try {
      const report = await api.phoneImport(chosen);
      for (const [src, err] of report.failed) addLog("Warn", `${src}: ${err}`);
      setStage({ kind: "done", report });
    } catch (e) {
      setStage({ kind: "error", message: String(e) });
    } finally {
      un();
    }
  };

  const convert = async (outputs: string[]) => {
    onClose();
    await addPaths(outputs);
  };

  const busy = stage.kind === "copying" || stage.kind === "planning";
  const close = () => {
    if (busy) void api.phoneCancel();
    onClose();
  };

  /** Выбор и галерея — во время поиска и после. */
  const gallery = (items: PhonePlanItem[]) => {
    const pickedBytes = [...picked].reduce((sum, i) => sum + (items[i]?.size ?? 0), 0);
    return (
      <>
        <div className="sticky -top-5 z-10 -mx-6 flex flex-wrap items-center gap-3 border-y border-line bg-accent-soft px-6 py-2">
          <span className="text-[16px] font-medium">
            Выбрано {picked.size} из {items.length} ({bytes(pickedBytes)})
          </span>
          <Button
            variant="ghost"
            disabled={picked.size === items.length}
            onClick={() => setPicked(new Set(items.map((_, i) => i)))}
          >
            Выбрать все
          </Button>
          <Button variant="ghost" disabled={picked.size === 0} onClick={() => setPicked(new Set())}>
            Снять все
          </Button>
        </div>
        <PhoneGallery items={items} selected={picked} onChange={setPicked} />
      </>
    );
  };

  let body: React.ReactNode;
  let footer: React.ReactNode;
  switch (stage.kind) {
    case "search":
      body = <p className="text-[18px]">Ищу телефон…</p>;
      break;
    case "none":
      body = (
        <div className="space-y-4 text-[18px] leading-relaxed">
          <p>Телефон не найден. Сделайте так:</p>
          <ol className="list-decimal space-y-2 pl-7">
            <li>Подключите телефон к компьютеру кабелем.</li>
            <li>Разблокируйте экран телефона.</li>
            <li>
              На Android опустите шторку уведомлений, нажмите на «Зарядка через USB» и выберите «Передача файлов».
            </li>
            <li>На iPhone нажмите «Доверять этому компьютеру» и введите код телефона.</li>
          </ol>
          <p>Затем нажмите «Искать снова».</p>
        </div>
      );
      footer = (
        <Button variant="primary" size="lg" onClick={() => void search()}>
          Искать снова
        </Button>
      );
      break;
    case "choose":
      body = (
        <div className="space-y-3">
          <p className="text-[18px]">Нашлось несколько устройств. С какого перенести?</p>
          {stage.sources.map((s, i) => (
            <Button
              key={s.name}
              size="lg"
              className="w-full justify-start"
              onClick={() => {
                setDevice({ index: i, name: s.name });
                if (dest) void plan(i, dest);
              }}
            >
              <Smartphone className="size-6" />
              {s.name}
              {s.kind === "card" && " (карта памяти)"}
            </Button>
          ))}
        </div>
      );
      break;
    case "planning": {
      const searching = (
        <p className="text-[18px] leading-relaxed">
          Смотрю, что есть на телефоне «{device?.name}»
          {stage.found > 0 && `: найдено ${stage.found} ${plural(stage.found, "снимок", "снимка", "снимков")}`}…
        </p>
      );
      body =
        stage.items.length === 0 ? (
          searching
        ) : (
          <div className="space-y-4 text-[18px] leading-relaxed">
            {searching}
            <p>
              Новые снимки появляются ниже — уже можно нажать на ненужные, чтобы не переносить их. Перенести можно
              будет, когда поиск закончится.
            </p>
            {gallery(stage.items)}
          </div>
        );
      footer = (
        <>
          <Button onClick={close}>Остановить</Button>
          {stage.items.length > 0 && (
            <Button variant="primary" size="lg" disabled>
              Перенести всё
            </Button>
          )}
        </>
      );
      break;
    }
    case "ready": {
      const p = stage.plan;
      body = (
        <div className="space-y-4 text-[18px] leading-relaxed">
          <p>
            Телефон: <span className="font-semibold">{device?.name}</span>
          </p>
          {p.count > 0 ? (
            <>
              <p>
                Можно перенести {media(p.photos, p.videos)} — <span className="font-semibold">{bytes(p.bytes)}</span>.
                {p.already > 0 && ` Ещё ${files(p.already)} уже перенесено раньше, их здесь нет.`} Нажмите на снимок,
                чтобы не переносить его.
              </p>
              {gallery(p.items)}
            </>
          ) : (
            <p>
              Всё уже перенесено: {files(p.already)} лежат на компьютере. Новых снимков на телефоне нет.
            </p>
          )}
          {p.unread > 0 && (
            <div className="space-y-2 rounded-lg bg-sunken px-4 py-3">
              <p>Часть телефона прочитать не получилось — возможно, здесь показано не всё.</p>
              <Button onClick={() => device && dest && void plan(device.index, dest)}>Искать снова</Button>
            </div>
          )}
          <div className="space-y-2 rounded-lg bg-sunken px-4 py-3">
            <p className="text-[16px] text-muted">Сохранить в папку (внутри — по месяцам):</p>
            <p className="text-[16px] break-all">{dest}</p>
            <Button onClick={() => void changeDest()}>Выбрать другую папку</Button>
          </div>
          <p className="text-[16px] text-muted">С телефона ничего не удалится.</p>
        </div>
      );
      footer =
        p.count > 0 ? (
          <Button variant="primary" size="lg" disabled={picked.size === 0} onClick={() => void start(p)}>
            {picked.size === p.count ? "Перенести всё" : `Перенести выбранные (${picked.size})`}
          </Button>
        ) : (
          <Button variant="primary" onClick={onClose}>
            Готово
          </Button>
        );
      break;
    }
    case "copying":
      body = (
        <div className="space-y-4">
          <p className="text-[18px]">
            Переношу: {stage.files} из {stage.count} ({bytes(stage.bytes)} из {bytes(stage.total)})
          </p>
          <ProgressBar value={stage.total ? stage.bytes / stage.total : 0} />
          <p className="text-[16px] text-muted">Не отключайте телефон, пока перенос не закончится.</p>
        </div>
      );
      footer = <Button onClick={() => void api.phoneCancel()}>Остановить</Button>;
      break;
    case "done": {
      const r = stage.report;
      body = (
        <div className="space-y-3 text-[18px] leading-relaxed">
          <p>
            {r.cancelled ? "Перенос остановлен. " : ""}Перенесено {files(r.copied)} ({bytes(r.bytes)}) в папку «{dest}».
          </p>
          {r.failed.length > 0 && (
            <p>
              Не получилось: {files(r.failed.length)}. Подробности — во вкладке «Подробности». Можно запустить перенос
              ещё раз: уже перенесённое повторяться не будет.
            </p>
          )}
          <p className="text-[16px] text-muted">С телефона ничего не удалено.</p>
        </div>
      );
      footer = (
        <>
          {dest && (
            <Button onClick={() => void openPath(dest)}>
              <FolderOpen className="size-5" />
              Открыть папку
            </Button>
          )}
          {r.outputs.length > 0 && (
            <Button variant="primary" onClick={() => void convert(r.outputs)}>
              Перевести их в обычные форматы
            </Button>
          )}
          <Button onClick={onClose}>Готово</Button>
        </>
      );
      break;
    }
    case "error":
      body = <p className="text-[18px] leading-relaxed">Не получилось: {stage.message}</p>;
      footer = (
        <Button variant="primary" onClick={() => void search()}>
          Попробовать ещё раз
        </Button>
      );
      break;
  }

  return (
    <Dialog
      open={isOpen}
      onOpenChange={(v) => !v && close()}
      title="Перенести фото с телефона"
      width={
        (stage.kind === "ready" && stage.plan.count > 0) || (stage.kind === "planning" && stage.items.length > 0)
          ? 1100
          : 640
      }
      footer={footer}
    >
      {body}
    </Dialog>
  );
}
