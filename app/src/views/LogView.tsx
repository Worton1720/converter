import { useVirtualizer } from "@tanstack/react-virtual";
import { save } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import { FileDown } from "lucide-react";
import { useMemo, useRef, useState } from "react";
import { Button, Select } from "../components/ui";
import { api } from "../lib/api";
import { useStore, type LogEntry } from "../lib/store";

type Level = "all" | "problems";

const time = (t: number) => new Date(t).toLocaleTimeString("ru-RU");
const LEVEL_TEXT: Record<LogEntry["level"], string> = { Info: "", Warn: "Внимание", Error: "Ошибка" };

export function LogView() {
  const log = useStore((s) => s.log);
  const logPath = useStore((s) => s.info?.log_path);
  const clearLog = useStore((s) => s.clearLog);
  const addLog = useStore((s) => s.addLog);
  const [level, setLevel] = useState<Level>("all");
  const ref = useRef<HTMLDivElement>(null);
  const shown = useMemo(() => (level === "all" ? log : log.filter((l) => l.level !== "Info")), [log, level]);
  const virtualizer = useVirtualizer({
    count: shown.length,
    getScrollElement: () => ref.current,
    estimateSize: () => 30,
    overscan: 20,
  });

  const exportLog = async () => {
    const path = await save({ title: "Сохранить журнал", defaultPath: "журнал.txt", filters: [{ name: "Текст", extensions: ["txt", "log"] }] });
    if (!path) return;
    const text = log
      .map((l) => `${new Date(l.time).toISOString()}\t${l.level}\t${l.path ?? ""}\t${l.message}`)
      .join("\n");
    await api.writeTextFile(path, text).catch((e) => addLog("Error", `Журнал не сохранён: ${e}`));
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-12 shrink-0 items-center gap-2 border-b border-line px-4">
        <div className="w-44">
          <Select<Level>
            value={level}
            onChange={setLevel}
            className="h-7 text-[14px]"
            options={[
              { value: "all", label: "Все записи" },
              { value: "problems", label: "Ошибки и предупреждения" },
            ]}
          />
        </div>
        <span className="text-[14px] text-muted tabular-nums">{shown.length}</span>
        <div className="ml-auto flex gap-2">
          {logPath && (
            <Button variant="ghost" onClick={() => void openPath(logPath)}>
              Открыть полный журнал
            </Button>
          )}
          <Button variant="ghost" disabled={!log.length} onClick={() => void exportLog()}>
            <FileDown className="size-5" />
            Сохранить в файл
          </Button>
          <Button variant="ghost" disabled={!log.length} onClick={clearLog}>
            Очистить
          </Button>
        </div>
      </div>
      {shown.length === 0 ? (
        <div className="flex flex-1 items-center justify-center text-[16px] text-muted">
          Здесь появятся подробности по каждому файлу, включая ошибки ffmpeg.
        </div>
      ) : (
        <div ref={ref} className="min-h-0 flex-1 overflow-y-auto selectable">
          <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
            {virtualizer.getVirtualItems().map((v) => {
              const l = shown[v.index];
              return (
                <div
                  key={l.n}
                  ref={virtualizer.measureElement}
                  data-index={v.index}
                  style={{ position: "absolute", top: 0, left: 0, right: 0, transform: `translateY(${v.start}px)` }}
                  className="flex gap-3 border-b border-line/60 px-4 py-1.5 text-[14px]"
                >
                  <span className="w-16 shrink-0 text-muted tabular-nums">{time(l.time)}</span>
                  <span
                    className={clsx(
                      "w-20 shrink-0",
                      l.level === "Error" && "text-err",
                      l.level === "Warn" && "text-warn",
                    )}
                  >
                    {LEVEL_TEXT[l.level]}
                  </span>
                  <span className="min-w-0 flex-1 break-words whitespace-pre-wrap">{l.message}</span>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
