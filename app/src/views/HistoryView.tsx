import { openPath } from "@tauri-apps/plugin-opener";
import { FolderOpen } from "lucide-react";
import { useEffect, useState } from "react";
import { Button, Dialog } from "../components/ui";
import { api } from "../lib/api";
import { basename, bytes, date, files } from "../lib/format";
import { useStore } from "../lib/store";
import type { HistoryItem, UndoReport } from "../lib/types";

/** Что произошло — одной-двумя фразами, без терминов (ТЗ 8.5). */
function describe(h: HistoryItem, running: boolean): string[] {
  const lines: string[] = [];
  if (h.kind === "convert") {
    let main = h.done ? `Преобразовано ${files(h.done)}` : "Ничего не преобразовано";
    if (h.failed) main += `, не получилось: ${h.failed}`;
    if (h.skipped) main += `, пропущено: ${h.skipped}`;
    lines.push(`${main}.`);
    if (h.trashed) lines.push(`Исходные файлы (${h.trashed}) убраны в корзину.`);
    if (h.deleted) lines.push(`Исходные файлы (${h.deleted}) удалены навсегда.`);
    if (h.finishedMs === null && running) lines.push("Идёт сейчас.");
    else if (h.finishedMs === null && h.pending)
      lines.push(`Программа закрылась посреди работы: не сделано ${files(h.pending)}.`);
  } else {
    lines.push(`Убрано одинаковых файлов: ${h.done}, освобождено ${bytes(h.freed)}.`);
    if (h.trashed) lines.push("Они в корзине — их можно вернуть.");
    if (h.deleted) lines.push("Они удалены навсегда.");
    if (h.moved) lines.push("Они перемещены в другую папку.");
  }
  if (h.undone) lines.push("Отменено: всё возвращено как было.");
  return lines;
}

/** Можно ли что-то вернуть: удалённое навсегда — нельзя. */
export const canUndo = (h: HistoryItem) =>
  !h.undone && h.finishedMs !== null && (h.kind === "convert" ? h.done > 0 : h.trashed + h.moved > 0);

/** Что произойдёт при «Вернуть как было» — до нажатия, простыми словами. */
function undoWords(h: HistoryItem): string {
  const parts =
    h.kind === "convert"
      ? [
          `Готовые файлы (${h.done}) уйдут в корзину.`,
          h.trashed ? "Исходные файлы вернутся из корзины на прежние места." : "",
        ]
      : ["Убранные файлы вернутся на прежние места."];
  if (h.deleted) parts.push("Файлы, удалённые навсегда, вернуть нельзя.");
  return parts.filter(Boolean).join(" ");
}

/** Итог отмены одной фразой; проблемы — в «Подробности». */
export async function undoEntry(id: number): Promise<string> {
  const r: UndoReport = await api.historyUndo(id);
  const addLog = useStore.getState().addLog;
  for (const [path, problem] of r.problems) addLog("Warn", `${basename(path)}: ${problem}`, path);
  const done = r.restored || r.removed ? "Готово, всё вернулось как было." : "Ничего не вернулось.";
  return r.problems.length
    ? `${done} Не получилось: ${files(r.problems.length)} — подробности во вкладке «Подробности».`
    : done;
}

/** Что программа делала с файлами: последние 100 действий, новые сверху. */
export function HistoryView() {
  const [items, setItems] = useState<HistoryItem[] | null>(null);
  const [confirm, setConfirm] = useState<HistoryItem | null>(null);
  const [result, setResult] = useState<{ id: number; text: string } | null>(null);
  const running = useStore((s) => s.run.active);
  const reload = () => api.historyList().then(setItems, () => setItems([]));

  useEffect(() => {
    void reload();
  }, [running]);

  const undo = async (h: HistoryItem) => {
    setConfirm(null);
    try {
      setResult({ id: h.id, text: await undoEntry(h.id) });
    } catch (e) {
      setResult({ id: h.id, text: `Не получилось: ${e}` });
    }
    void reload();
  };

  if (items === null) return null;
  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto max-w-[760px] space-y-4 px-6 py-8">
        <h2 className="text-[22px] font-semibold">Что программа делала с файлами</h2>
        {items.length === 0 ? (
          <p className="text-[18px] text-muted">
            Пока ничего. Здесь появится всё, что программа сделает с вашими фото и видео.
          </p>
        ) : (
          items.map((h, i) => (
            <article key={h.id} className="space-y-3 rounded-lg border border-line bg-surface px-5 py-4">
              <div className="text-[14px] text-muted">
                {date(h.id)} — {h.kind === "convert" ? "преобразование" : "одинаковые файлы"}
              </div>
              {describe(h, running && i === 0).map((line) => (
                <p key={line} className="text-[16px] leading-relaxed">
                  {line}
                </p>
              ))}
              {result?.id === h.id && <p className="rounded-lg bg-accent-soft px-4 py-3 text-[16px]">{result.text}</p>}
              <div className="flex flex-wrap gap-3">
                {h.outputDirs.length > 0 && !h.undone && (
                  <Button onClick={() => void openPath(h.outputDirs[0])}>
                    <FolderOpen className="size-5" />
                    Открыть папку с результатом
                  </Button>
                )}
                {canUndo(h) && (
                  <Button disabled={running} onClick={() => setConfirm(h)}>
                    Вернуть как было
                  </Button>
                )}
              </div>
            </article>
          ))
        )}
      </div>
      <Dialog
        open={confirm !== null}
        onOpenChange={(v) => !v && setConfirm(null)}
        title="Вернуть как было?"
        width={480}
        footer={
          <>
            <Button variant="primary" onClick={() => confirm && void undo(confirm)}>
              Вернуть как было
            </Button>
            <Button onClick={() => setConfirm(null)}>Не надо</Button>
          </>
        }
      >
        <p className="text-[16px] leading-relaxed">{confirm && undoWords(confirm)}</p>
      </Dialog>
    </div>
  );
}
