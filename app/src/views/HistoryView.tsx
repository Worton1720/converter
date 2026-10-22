import { openPath } from "@tauri-apps/plugin-opener";
import { FolderOpen } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "../components/ui";
import { api } from "../lib/api";
import { bytes, date, files } from "../lib/format";
import { useStore } from "../lib/store";
import type { HistoryItem } from "../lib/types";

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

/** Что программа делала с файлами: последние 100 действий, новые сверху. */
export function HistoryView() {
  const [items, setItems] = useState<HistoryItem[] | null>(null);
  const running = useStore((s) => s.run.active);

  useEffect(() => {
    api.historyList().then(setItems, () => setItems([]));
  }, [running]);

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
              {h.outputDirs.length > 0 && (
                <div className="flex flex-wrap gap-3">
                  <Button onClick={() => void openPath(h.outputDirs[0])}>
                    <FolderOpen className="size-5" />
                    Открыть папку с результатом
                  </Button>
                </div>
              )}
            </article>
          ))
        )}
      </div>
    </div>
  );
}
