import clsx from "clsx";
import { Image as ImageIcon, Play } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Button, Checkbox } from "../../components/ui";
import { api } from "../../lib/api";
import { bytes } from "../../lib/format";
import type { PhonePlanItem } from "../../lib/types";
import { Thumb } from "./FileTable";

const MONTHS = [
  "Январь",
  "Февраль",
  "Март",
  "Апрель",
  "Май",
  "Июнь",
  "Июль",
  "Август",
  "Сентябрь",
  "Октябрь",
  "Ноябрь",
  "Декабрь",
];

/** «2026-10» → «Октябрь 2026». */
export function monthTitle(month: string): string {
  const m = /^(\d{4})-(\d{2})$/.exec(month);
  return m ? `${MONTHS[Number(m[2]) - 1]} ${m[1]}` : month;
}

/** Превью снимка с телефона: запрашивается, только когда плитка видна на экране —
 *  у Android каждое превью читается с телефона. */
function PhoneThumb({ index, video }: { index: number; video: boolean }) {
  const ref = useRef<HTMLDivElement>(null);
  const [path, setPath] = useState<string | null | undefined>(undefined);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const io = new IntersectionObserver(
      ([e]) => {
        if (e.isIntersecting) {
          io.disconnect();
          api.phonePreview(index).then(setPath, () => setPath(null));
        }
      },
      { rootMargin: "200px" },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [index]);
  return (
    <div ref={ref} className="relative size-[132px] overflow-hidden rounded-lg bg-sunken">
      {path ? (
        <Thumb path={path} kind={video ? "video" : "image"} size={132} />
      ) : (
        path === null &&
        (video ? (
          <Play className="absolute inset-0 m-auto size-10 fill-faint text-faint" />
        ) : (
          <ImageIcon className="absolute inset-0 m-auto size-8 text-faint" />
        ))
      )}
    </div>
  );
}

/** Галерея нового на телефоне по месяцам: выбор нажатием (Shift — диапазон), «весь месяц». */
export function PhoneGallery({
  items,
  selected,
  onChange,
}: {
  items: PhonePlanItem[];
  selected: Set<number>;
  onChange: (next: Set<number>) => void;
}) {
  const anchor = useRef<number | null>(null);
  // Свежие месяцы сверху, внутри месяца — по имени (у камер оно растёт со временем съёмки).
  const months = useMemo(() => {
    const by = new Map<string, number[]>();
    items.forEach((it, i) => by.set(it.month, [...(by.get(it.month) ?? []), i]));
    return [...by.entries()]
      .sort(([a], [b]) => b.localeCompare(a))
      .map(([month, idx]) => ({ month, idx: idx.sort((a, b) => items[a].name.localeCompare(items[b].name)) }));
  }, [items]);
  const order = useMemo(() => months.flatMap((m) => m.idx), [months]);

  const set = (idx: number[], on: boolean) => {
    const next = new Set(selected);
    for (const i of idx) {
      if (on) next.add(i);
      else next.delete(i);
    }
    onChange(next);
  };
  const click = (i: number, range: boolean) => {
    const a = anchor.current === null ? -1 : order.indexOf(anchor.current);
    const b = order.indexOf(i);
    if (range && a >= 0) set(order.slice(Math.min(a, b), Math.max(a, b) + 1), true);
    else set([i], !selected.has(i));
    anchor.current = i;
  };

  return (
    <div className="space-y-6">
      {months.map(({ month, idx }) => {
        const all = idx.every((i) => selected.has(i));
        return (
          <section key={month} className="space-y-3">
            <div className="flex flex-wrap items-center gap-3">
              <h3 className="text-[18px] font-semibold">
                {monthTitle(month)} <span className="font-normal text-muted">— {idx.length}</span>
              </h3>
              <Button variant="ghost" onClick={() => set(idx, !all)}>
                {all ? "Снять весь месяц" : "Выбрать весь месяц"}
              </Button>
            </div>
            <div className="flex flex-wrap gap-3">
              {idx.map((i) => {
                const it = items[i];
                const on = selected.has(i);
                return (
                  <div
                    key={i}
                    role="button"
                    aria-pressed={on}
                    aria-label={`${it.name}${on ? ", выбрано" : ""}`}
                    onClick={(e) => click(i, e.shiftKey)}
                    className={clsx(
                      "relative w-[148px] cursor-pointer space-y-1 rounded-xl border-2 p-2",
                      on ? "border-accent bg-accent-soft" : "border-line bg-surface opacity-70",
                    )}
                  >
                    <PhoneThumb index={i} video={it.video} />
                    <Checkbox
                      tone="select"
                      checked={on}
                      onChange={() => click(i, false)}
                      label={`Выбрать ${it.name}`}
                      className="absolute top-1 left-1 bg-surface/80"
                    />
                    <div className="truncate text-[14px]">{it.name}</div>
                    <div className="text-[14px] text-muted">
                      {it.video ? "Видео, " : ""}
                      {bytes(it.size)}
                    </div>
                  </div>
                );
              })}
            </div>
          </section>
        );
      })}
    </div>
  );
}
