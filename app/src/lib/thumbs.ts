import { useEffect, useState } from "react";
import { thumbUrl } from "./api";

// WebKitGTK не грузит <img> из нестандартной схемы на странице другого источника,
// а fetch с CORS проходит. Поэтому миниатюры идут через fetch → blob-URL.
const MAX_CACHED = 3000;
const cache = new Map<string, Promise<string>>();

function load(path: string, size?: number): Promise<string> {
  const key = `${size ?? 0}|${path}`;
  const hit = cache.get(key);
  if (hit) {
    // Обновляем позицию в LRU.
    cache.delete(key);
    cache.set(key, hit);
    return hit;
  }
  const p = fetch(thumbUrl(path, size))
    .then((r) => (r.ok ? r.blob() : Promise.reject(new Error(String(r.status)))))
    .then((b) => URL.createObjectURL(b));
  p.catch(() => cache.delete(key));
  cache.set(key, p);
  while (cache.size > MAX_CACHED) {
    const [oldKey, old] = cache.entries().next().value!;
    cache.delete(oldKey);
    void old.then(URL.revokeObjectURL, () => {});
  }
  return p;
}

/** blob-URL миниатюры; `null` — ещё грузится, `false` — не получилось. */
export function useThumb(path: string, size?: number): string | null | false {
  const [src, setSrc] = useState<string | null | false>(null);
  useEffect(() => {
    let alive = true;
    setSrc(null);
    load(path, size).then(
      (url) => alive && setSrc(url),
      () => alive && setSrc(false),
    );
    return () => {
      alive = false;
    };
  }, [path, size]);
  return src;
}
