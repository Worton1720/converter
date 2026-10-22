import { useVirtualizer } from "@tanstack/react-virtual";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import { FolderOpen, Image as ImageIcon, Play, X } from "lucide-react";
import { useMemo, useRef } from "react";
import { IconButton } from "../../components/ui";
import { api } from "../../lib/api";
import { basename, bytes, dirname, duration, megapixels, plainError } from "../../lib/format";
import { useStore, type Row } from "../../lib/store";
import { useThumb } from "../../lib/thumbs";

const COLS = "grid-cols-[48px_minmax(0,1fr)_84px_96px_170px_190px_150px]";

export function Thumb({ path, kind, size = 44 }: { path: string; kind: "image" | "video" | null; size?: number }) {
  const src = useThumb(path);
  return (
    <div className="relative shrink-0 overflow-hidden rounded-lg bg-sunken" style={{ width: size, height: size }}>
      {src && <img src={src} alt="" draggable={false} className="size-full object-cover" />}
      {src === false && <ImageIcon className="absolute inset-0 m-auto size-6 text-faint" />}
      {kind === "video" && src && (
        <span className="absolute right-1 bottom-1 rounded bg-black/60 p-1">
          <Play className={clsx("fill-white text-white", size > 60 ? "size-5" : "size-3")} />
        </span>
      )}
    </div>
  );
}

/** Две строки: состояние и пояснение. Ничего важного не прячется во всплывающие подсказки (ТЗ 8.5). */
function Status({ row }: { row: Row }) {
  const { job, item } = row;
  const skipLive = useStore((s) => s.settings?.convert.skip_live_photo_video ?? true);
  const line = (main: React.ReactNode, sub?: string, cls = "text-muted") => (
    <div className="min-w-0">
      <div className={clsx("truncate text-[14px] font-medium", cls)}>{main}</div>
      {sub && <div className="line-clamp-1 text-[14px] break-all text-muted">{sub}</div>}
    </div>
  );
  switch (job.status) {
    case "running":
      return (
        <div className="w-full space-y-1">
          <div className="flex justify-between text-[14px] tabular-nums">
            <span className="text-accent">{Math.round(job.progress * 100)} %</span>
            {job.speed != null && job.speed > 0 && <span className="text-muted">×{job.speed.toFixed(1)}</span>}
          </div>
          <div className="progress-track">
            <div className="progress-fill" style={{ width: `${job.progress * 100}%` }} />
          </div>
        </div>
      );
    case "done":
      return line("Готово", job.output ? basename(job.output) : job.operation, "text-ok");
    case "skipped":
      return line("Пропущено", job.message);
    case "failed":
      return line("Не получилось", plainError(job.message), "text-err");
    case "cancelled":
      return line("Отменено");
    default:
      if (item.error) return line("Не читается", plainError(item.error), "text-err");
      if (item.live === "video" && skipLive) return line("Пропустится", "часть «живого» фото");
      return line(item.inspected ? "Ожидает" : "Читаю свойства…", undefined, "text-faint");
  }
}

function Details({ row }: { row: Row }) {
  const { item } = row;
  if (item.width == null || item.height == null) return <span className="text-faint">—</span>;
  return (
    <span className="flex min-w-0 items-baseline gap-2 tabular-nums">
      <span>
        {item.width}×{item.height}
      </span>
      <span className="text-[14px] text-muted">
        {item.kind === "video" && item.duration != null ? duration(item.duration) : megapixels(item.width, item.height)}
      </span>
      {item.hdr && (
        <span className="rounded-[3px] border border-line-strong px-1 text-[14px] leading-4 text-muted">HDR</span>
      )}
    </span>
  );
}

export function FileTable({ paths }: { paths: string[] }) {
  const rows = useStore((s) => s.rows);
  const selected = useStore((s) => s.selected);
  const select = useStore((s) => s.select);
  const removePaths = useStore((s) => s.removePaths);
  const runActive = useStore((s) => s.run.active);
  const parentRef = useRef<HTMLDivElement>(null);
  const anchor = useRef<string | null>(null);

  const index = useMemo(() => new Map(paths.map((p, i) => [p, i])), [paths]);
  const virtualizer = useVirtualizer({
    count: paths.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 64,
    overscan: 12,
  });

  const onRowClick = (path: string, e: React.MouseEvent) => {
    if (e.shiftKey && anchor.current && index.has(anchor.current)) {
      const a = index.get(anchor.current)!;
      const b = index.get(path)!;
      const [from, to] = a < b ? [a, b] : [b, a];
      select(paths.slice(from, to + 1), "set");
      return;
    }
    anchor.current = path;
    select([path], e.ctrlKey || e.metaKey ? "toggle" : "set");
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if ((e.key === "Delete" || e.key === "Backspace") && selected.size > 0) {
      const removable = [...selected].filter((p) => rows[p]?.job.status !== "running");
      if (!runActive || removable.length) removePaths(removable);
      e.preventDefault();
    } else if (e.key === "a" && (e.ctrlKey || e.metaKey)) {
      select(paths, "set");
      e.preventDefault();
    } else if (e.key === "Escape") {
      select([], "set");
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col" onKeyDown={onKeyDown} tabIndex={0} aria-label="Список файлов">
      <div className={clsx("grid shrink-0 gap-3 border-b border-line px-4 py-2 text-[14px] text-muted", COLS)}>
        <span />
        <span>Файл</span>
        <span>Тип</span>
        <span className="text-right">Размер</span>
        <span>Разрешение</span>
        <span>Состояние</span>
        <span />
      </div>
      <div ref={parentRef} className="min-h-0 flex-1 overflow-y-auto">
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {virtualizer.getVirtualItems().map((v) => {
            const path = paths[v.index];
            const row = rows[path];
            if (!row) return null;
            const isSel = selected.has(path);
            const { item, job } = row;
            return (
              <div
                key={path}
                onClick={(e) => onRowClick(path, e)}
                style={{ position: "absolute", top: 0, left: 0, right: 0, height: v.size, transform: `translateY(${v.start}px)` }}
                className={clsx(
                  "grid items-center gap-3 border-b border-line/60 px-4",
                  COLS,
                  isSel ? "bg-accent-soft" : "hover:bg-sunken/70",
                )}
              >
                <Thumb path={item.path} kind={item.kind} />
                <div className="min-w-0">
                  <div className="truncate text-[16px]">
                    {item.name}
                    {item.live === "photo" && <span className="ml-2 text-[14px] text-muted">«живое» фото</span>}
                  </div>
                  <div className="truncate text-[14px] text-faint">{dirname(item.path)}</div>
                </div>
                <span className="truncate text-[14px] text-muted">{item.format}</span>
                <span className="text-right text-[14px] tabular-nums">{bytes(item.size)}</span>
                <span className="min-w-0 text-[14px]">
                  <Details row={row} />
                </span>
                <div className="flex min-w-0 items-center">
                  <Status row={row} />
                </div>
                <div className="flex justify-end">
                  {job.status === "running" ? (
                    <IconButton label="Отменить" onClick={(e) => (e.stopPropagation(), void api.cancelItem(path))}>
                      <X className="size-5" />
                    </IconButton>
                  ) : job.status === "done" && job.output ? (
                    <IconButton
                      label="В папке"
                      onClick={(e) => (e.stopPropagation(), void revealItemInDir(job.output!))}
                    >
                      <FolderOpen className="size-5" />
                    </IconButton>
                  ) : (
                    !runActive && (
                      <IconButton label="Убрать" onClick={(e) => (e.stopPropagation(), removePaths([path]))}>
                        <X className="size-5" />
                      </IconButton>
                    )
                  )}
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
