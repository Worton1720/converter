import { useEffect, useRef, useState, type RefObject } from "react";

/** Прямоугольник в координатах содержимого (вместе с прокруткой). */
export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export const intersects = (r: Rect, x: number, y: number, w: number, h: number) =>
  r.x < x + w && r.x + r.w > x && r.y < y + h && r.y + r.h > y;

/** Сколько надо сдвинуть мышь, чтобы нажатие стало рамкой, а не щелчком. */
const DRAG_START = 6;
/** У края прокручиваемой области — автопрокрутка. */
const EDGE = 48;

function scrollParent(el: HTMLElement | null): HTMLElement | null {
  for (let p = el; p; p = p.parentElement) {
    const o = getComputedStyle(p).overflowY;
    if ((o === "auto" || o === "scroll") && p.scrollHeight > p.clientHeight) return p;
  }
  return null;
}

/**
 * Выделение рамкой: зажать левую кнопку и вести мышь. Без клавиш — выделяется попавшее в рамку,
 * с Shift — добавляется к выбранному, с Ctrl — снимается с выбранного. Esc — отменить.
 * Короткое нажатие без движения остаётся обычным щелчком. Только дополнение к галочкам (ТЗ 8.5).
 *
 * `area` ловит нажатие (может быть больше содержимого), `content` — система координат,
 * `hits` — что попало в прямоугольник (по геометрии: в виртуальных списках плиток нет в DOM).
 */
export function useMarquee(opts: {
  area: RefObject<HTMLElement | null>;
  content: RefObject<HTMLElement | null>;
  hits: (r: Rect) => string[];
  selected: () => Set<string>;
  apply: (next: Set<string>) => void;
}) {
  const [box, setBox] = useState<Rect | null>(null);
  const live = useRef(opts);
  live.current = opts;
  const drag = useRef<{
    start: { x: number; y: number };
    client: { x: number; y: number };
    mode: "replace" | "add" | "remove";
    base: Set<string>;
    active: boolean;
    scroller: HTMLElement | null;
    raf: number;
  } | null>(null);

  const point = (cx: number, cy: number) => {
    const r = live.current.content.current!.getBoundingClientRect();
    return { x: cx - r.left, y: cy - r.top };
  };

  const update = () => {
    const d = drag.current;
    if (!d || !d.active) return;
    const p = point(d.client.x, d.client.y);
    const rect = {
      x: Math.min(d.start.x, p.x),
      y: Math.min(d.start.y, p.y),
      w: Math.abs(p.x - d.start.x),
      h: Math.abs(p.y - d.start.y),
    };
    setBox(rect);
    const hit = live.current.hits(rect);
    const next = d.mode === "replace" ? new Set<string>() : new Set(d.base);
    for (const id of hit) {
      if (d.mode === "remove") next.delete(id);
      else next.add(id);
    }
    live.current.apply(next);
  };

  // Автопрокрутка, пока мышь у края и кнопка зажата.
  const tick = () => {
    const d = drag.current;
    if (!d) return;
    if (d.active && d.scroller) {
      const r = d.scroller.getBoundingClientRect();
      const y = d.client.y;
      const speed = y < r.top + EDGE ? -(r.top + EDGE - y) : y > r.bottom - EDGE ? y - (r.bottom - EDGE) : 0;
      if (speed) {
        d.scroller.scrollTop += Math.max(-30, Math.min(30, speed / 2));
        update();
      }
    }
    d.raf = requestAnimationFrame(tick);
  };

  const finish = (cancel: boolean) => {
    const d = drag.current;
    if (!d) return;
    cancelAnimationFrame(d.raf);
    if (cancel && d.active) live.current.apply(d.base);
    if (d.active) {
      // Отпускание после рамки не должно стать щелчком по плитке.
      const swallow = (e: MouseEvent) => {
        e.stopPropagation();
        e.preventDefault();
      };
      window.addEventListener("click", swallow, { capture: true, once: true });
      setTimeout(() => window.removeEventListener("click", swallow, { capture: true }), 0);
    }
    drag.current = null;
    setBox(null);
  };

  useEffect(() => {
    const move = (e: PointerEvent) => {
      const d = drag.current;
      if (!d) return;
      d.client = { x: e.clientX, y: e.clientY };
      if (!d.active) {
        const p = point(e.clientX, e.clientY);
        if (Math.hypot(p.x - d.start.x, p.y - d.start.y) < DRAG_START) return;
        d.active = true;
        document.getSelection()?.removeAllRanges();
      }
      update();
    };
    const up = () => finish(false);
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape" && drag.current) finish(true);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
    window.addEventListener("keydown", key);
    return () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      window.removeEventListener("keydown", key);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onPointerDown = (e: React.PointerEvent) => {
    if (e.button !== 0 || !live.current.content.current) return;
    // Галочки и кнопки работают как обычно.
    if ((e.target as HTMLElement).closest("button, input, a, [role=checkbox]")) return;
    e.preventDefault(); // без выделения текста при протягивании
    const area = live.current.area.current;
    drag.current = {
      start: point(e.clientX, e.clientY),
      client: { x: e.clientX, y: e.clientY },
      mode: e.shiftKey ? "add" : e.ctrlKey || e.metaKey ? "remove" : "replace",
      base: new Set(live.current.selected()),
      active: false,
      scroller: scrollParent(area),
      raf: 0,
    };
    drag.current.raf = requestAnimationFrame(tick);
  };

  return { box, onPointerDown };
}

/** Рамка поверх содержимого (`content` должен быть `position: relative`). */
export function marqueeStyle(box: Rect): React.CSSProperties {
  return { position: "absolute", left: box.x, top: box.y, width: box.w, height: box.h, zIndex: 20, pointerEvents: "none" };
}
