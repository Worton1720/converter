import { useVirtualizer } from "@tanstack/react-virtual";
import clsx from "clsx";
import { Check } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Checkbox } from "../../components/ui";
import { intersects, marqueeStyle, useMarquee } from "../../lib/marquee";
import { useStore, type Row } from "../../lib/store";
import { Thumb } from "./FileTable";

const TILE = 168;
const TILE_HEIGHT = 250;
const GAP = 16;

/** Подпись состояния словами — без процентов, скоростей и кодов (ТЗ 8.5). */
function TileStatus({ row }: { row: Row }) {
  const skipLive = useStore((s) => s.settings?.convert.skip_live_photo_video ?? true);
  const { job, item } = row;
  switch (job.status) {
    case "running":
      return (
        <div className="space-y-1.5">
          <span className="text-[14px] text-accent">Обрабатывается</span>
          <div className="h-2 overflow-hidden rounded-full bg-line">
            <div className="h-full rounded-full bg-accent transition-[width] duration-200" style={{ width: `${job.progress * 100}%` }} />
          </div>
        </div>
      );
    case "done":
      return (
        <span className="flex items-center gap-1 text-[14px] font-medium text-ok">
          <Check className="size-4" strokeWidth={3} />
          Готово
        </span>
      );
    case "skipped":
      return <span className="text-[14px] text-muted">Пропущено</span>;
    case "failed":
      return <span className="text-[14px] font-medium text-err">Не получилось</span>;
    case "cancelled":
      return <span className="text-[14px] text-muted">Отменено</span>;
    default:
      if (item.error) return <span className="text-[14px] text-err">Файл не читается</span>;
      if (item.live === "video" && skipLive) {
        return <span className="block text-[14px] leading-snug text-muted">Пропустим: это часть «живого» фото</span>;
      }
      return <span className="text-[14px] text-muted">{item.kind === "video" ? "Видео" : "Фото"}</span>;
  }
}

export function FileGrid({ paths }: { paths: string[] }) {
  const rows = useStore((s) => s.rows);
  const selected = useStore((s) => s.selected);
  const clickSelect = useStore((s) => s.clickSelect);
  const parentRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const [columns, setColumns] = useState(4);

  useEffect(() => {
    const el = parentRef.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => {
      setColumns(Math.max(1, Math.floor((e.contentRect.width - 48 + GAP) / (TILE + GAP))));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const lines = Math.ceil(paths.length / columns);
  // Рамка: попадание по геометрии сетки — плиток вне экрана в DOM нет.
  const marquee = useMarquee({
    area: parentRef,
    content: contentRef,
    hits: (r) =>
      paths.filter((_, i) =>
        intersects(r, (i % columns) * (TILE + GAP), Math.floor(i / columns) * (TILE_HEIGHT + GAP), TILE, TILE_HEIGHT),
      ),
    selected: () => useStore.getState().selected,
    apply: (next) => useStore.getState().select([...next], "set"),
  });
  const virtualizer = useVirtualizer({
    count: lines,
    getScrollElement: () => parentRef.current,
    estimateSize: () => TILE_HEIGHT + GAP,
    overscan: 4,
  });

  return (
    <div ref={parentRef} onPointerDown={marquee.onPointerDown} className="min-h-0 flex-1 overflow-y-auto px-6 pt-2 pb-6">
      <div ref={contentRef} style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
        {marquee.box && (
          <div style={marqueeStyle(marquee.box)} className="rounded border-2 border-accent bg-accent/15" />
        )}
        {virtualizer.getVirtualItems().map((v) => (
          <div
            key={v.index}
            style={{ position: "absolute", top: 0, left: 0, right: 0, transform: `translateY(${v.start}px)`, gap: GAP }}
            className="flex"
          >
            {paths.slice(v.index * columns, v.index * columns + columns).map((path) => {
              const row = rows[path];
              if (!row) return null;
              const isSel = selected.has(path);
              // Нажатие по картинке выбирает её (ТЗ 8.5: без Ctrl); Shift — диапазон.
              return (
                <div
                  key={path}
                  role="button"
                  aria-pressed={isSel}
                  aria-label={`${row.item.name}${isSel ? ", выбрано" : ""}`}
                  onClick={(e) => clickSelect(path, paths, e.shiftKey)}
                  style={{ width: TILE, height: TILE_HEIGHT }}
                  className={clsx(
                    "relative flex shrink-0 cursor-pointer flex-col gap-2 rounded-xl border-2 bg-surface p-2",
                    isSel ? "border-accent bg-accent-soft" : row.job.status === "failed" ? "border-err" : "border-line",
                  )}
                >
                  <Thumb path={path} kind={row.item.kind} size={150} />
                  <Checkbox
                    tone="select"
                    checked={isSel}
                    onChange={() => clickSelect(path, paths, false)}
                    label={`Выбрать ${row.item.name}`}
                    className="absolute top-1 left-1 bg-surface/80"
                  />
                  <div className="min-w-0 px-1">
                    <div className="truncate text-[14px]" title={row.item.name}>
                      {row.item.name}
                    </div>
                    <TileStatus row={row} />
                  </div>
                </div>
              );
            })}
          </div>
        ))}
      </div>
    </div>
  );
}
