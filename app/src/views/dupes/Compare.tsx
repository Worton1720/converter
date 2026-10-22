import { useRef, useState } from "react";
import { Button, Dialog, Select } from "../../components/ui";
import { basename, bytes, date, dirname } from "../../lib/format";
import { useThumb } from "../../lib/thumbs";
import type { DupGroup } from "../../lib/types";

type Zoom = "1" | "2" | "4";

function Side({
  path,
  zoom,
  marked,
  scrollRef,
  onScroll,
  onToggle,
  meta,
}: {
  path: string;
  zoom: number;
  marked: boolean;
  scrollRef: React.RefObject<HTMLDivElement | null>;
  onScroll: () => void;
  onToggle: () => void;
  meta: string;
}) {
  const src = useThumb(path, 1600);
  return (
    <div className="flex min-w-0 flex-col gap-3">
      <div className="min-w-0">
        <div className="truncate text-[16px] font-medium">{basename(path)}</div>
        <div className="truncate text-[14px] text-muted">{dirname(path)}</div>
        <div className="text-[14px] text-muted tabular-nums">{meta}</div>
      </div>
      <div ref={scrollRef} onScroll={onScroll} className="h-[48vh] overflow-auto rounded-lg bg-sunken">
        {src ? (
          <img src={src} alt="" draggable={false} style={{ width: `${zoom * 100}%` }} className="max-w-none" />
        ) : (
          <div className="flex h-full items-center justify-center text-[16px] text-muted">
            {src === false ? "Превью недоступно" : "Загружаю…"}
          </div>
        )}
      </div>
      <div className="flex items-center justify-between gap-3">
        <span className={marked ? "text-[16px] font-medium text-err" : "text-[16px] font-medium text-ok"}>
          {marked ? "Эта копия будет убрана" : "Эта копия останется"}
        </span>
        <Button onClick={onToggle}>{marked ? "Оставить эту копию" : "Убрать эту копию"}</Button>
      </div>
    </div>
  );
}

/** Две копии рядом с общим увеличением и синхронной прокруткой (FR-10.2). */
export function CompareDialog({
  open,
  onClose,
  group,
  focus,
  marks,
  onToggle,
}: {
  open: boolean;
  onClose: () => void;
  group: DupGroup | undefined;
  focus: number;
  marks: boolean[];
  onToggle: (f: number) => void;
}) {
  const [zoom, setZoom] = useState<Zoom>("1");
  const [offset, setOffset] = useState(1);
  const left = useRef<HTMLDivElement>(null);
  const right = useRef<HTMLDivElement>(null);
  const syncing = useRef(false);
  if (!group || group.files.length < 2) return null;

  const n = group.files.length;
  const other = (focus + offset) % n === focus ? (focus + 1) % n : (focus + offset) % n;
  const sync = (from: React.RefObject<HTMLDivElement | null>, to: React.RefObject<HTMLDivElement | null>) => () => {
    if (syncing.current || !from.current || !to.current) return;
    syncing.current = true;
    to.current.scrollLeft = from.current.scrollLeft;
    to.current.scrollTop = from.current.scrollTop;
    requestAnimationFrame(() => (syncing.current = false));
  };
  const meta = (f: number) => {
    const file = group.files[f];
    return `${file.format ? `${file.format}, ` : ""}${bytes(file.size)}, изменён ${date(file.mtimeMs)}`;
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(v) => !v && onClose()}
      title="Сравнение копий"
      width={1240}
      footer={
        <>
          <div className="mr-auto flex items-center gap-3">
            <span className="text-[16px]">Увеличение</span>
            <div className="w-56">
              <Select<Zoom>
                value={zoom}
                onChange={setZoom}
                options={[
                  { value: "1", label: "По размеру окна" },
                  { value: "2", label: "В 2 раза" },
                  { value: "4", label: "В 4 раза" },
                ]}
              />
            </div>
          </div>
          {n > 2 && <Button onClick={() => setOffset((o) => (o % (n - 1)) + 1)}>Показать другую копию</Button>}
          <Button variant="primary" onClick={onClose}>
            Готово
          </Button>
        </>
      }
    >
      <p className="mb-4 text-[16px] text-muted">
        {group.kind === "Formats"
          ? "Это одно и то же фото в разных форматах: файлы не одинаковые, качество может немного отличаться. Увеличьте, чтобы сравнить, — обе картинки двигаются вместе."
          : "Содержимое этих файлов полностью совпадает. При увеличении обе картинки двигаются вместе."}
      </p>
      <div className="grid grid-cols-2 gap-5">
        <Side
          path={group.files[focus].path}
          zoom={Number(zoom)}
          marked={marks[focus] ?? false}
          scrollRef={left}
          onScroll={sync(left, right)}
          onToggle={() => onToggle(focus)}
          meta={meta(focus)}
        />
        <Side
          path={group.files[other].path}
          zoom={Number(zoom)}
          marked={marks[other] ?? false}
          scrollRef={right}
          onScroll={sync(right, left)}
          onToggle={() => onToggle(other)}
          meta={meta(other)}
        />
      </div>
    </Dialog>
  );
}
