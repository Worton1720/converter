const UNITS = ["Б", "КБ", "МБ", "ГБ", "ТБ"];

export function bytes(n: number): string {
  if (n < 1024) return `${n} Б`;
  let v = n;
  let u = 0;
  while (v >= 1024 && u < UNITS.length - 1) {
    v /= 1024;
    u++;
  }
  return `${v.toLocaleString("ru-RU", { maximumFractionDigits: v < 10 ? 1 : 0 })} ${UNITS[u]}`;
}

export function duration(sec: number): string {
  const s = Math.round(sec);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  const pad = (x: number) => x.toString().padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(r)}` : `${m}:${pad(r)}`;
}

/** «≈ 2 мин», «≈ 40 с». */
export function eta(sec: number): string {
  if (!isFinite(sec) || sec <= 0) return "";
  if (sec < 60) return `≈ ${Math.max(1, Math.round(sec / 5) * 5)} с`;
  if (sec < 3600) return `≈ ${Math.round(sec / 60)} мин`;
  const h = Math.floor(sec / 3600);
  const m = Math.round((sec % 3600) / 60);
  return `≈ ${h} ч ${m} мин`;
}

export function megapixels(w: number, h: number): string {
  return `${((w * h) / 1e6).toLocaleString("ru-RU", { maximumFractionDigits: 1 })} Мп`;
}

/** Склонение: plural(3, "файл", "файла", "файлов"). */
export function plural(n: number, one: string, few: string, many: string): string {
  const m10 = n % 10;
  const m100 = n % 100;
  if (m10 === 1 && m100 !== 11) return one;
  if (m10 >= 2 && m10 <= 4 && (m100 < 12 || m100 > 14)) return few;
  return many;
}

export function files(n: number): string {
  return `${n.toLocaleString("ru-RU")} ${plural(n, "файл", "файла", "файлов")}`;
}

export function dirname(path: string): string {
  const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return i > 0 ? path.slice(0, i) : path;
}

export function basename(path: string): string {
  const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return i >= 0 ? path.slice(i + 1) : path;
}

export function date(ms: number): string {
  return new Date(ms).toLocaleString("ru-RU", {
    day: "numeric",
    month: "short",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** «примерно 2 минуты», «меньше минуты» — для прогресса без сокращений (ТЗ 8.5). */
export function etaWords(sec: number): string {
  if (!isFinite(sec) || sec <= 0) return "";
  if (sec < 60) return "меньше минуты";
  const min = Math.round(sec / 60);
  if (min < 60) return `примерно ${min} ${plural(min, "минута", "минуты", "минут")}`;
  const h = Math.floor(min / 60);
  const m = min % 60;
  const hours = `${h} ${plural(h, "час", "часа", "часов")}`;
  return m ? `примерно ${hours} ${m} ${plural(m, "минута", "минуты", "минут")}` : `примерно ${hours}`;
}

/** «3 фото и 2 видео». */
export function mediaCount(images: number, videos: number): string {
  const parts = [];
  if (images) parts.push(`${images} фото`);
  if (videos) parts.push(`${videos} видео`);
  return parts.join(" и ") || "ничего";
}

/** Причина ошибки простыми словами; технические подробности — во вкладке «Подробности». */
export function plainError(message: string | undefined): string {
  const m = (message ?? "").toLowerCase();
  if (m.includes("space") || m.includes("мест")) return "не хватило места на диске";
  if (m.includes("не поддерживается") || m.includes("ffprobe") || m.includes("декодирован") || m.includes("не читается"))
    return "файл не удалось прочитать — возможно, он повреждён";
  if (m.includes("слишком большое")) return "фото слишком большое для этого формата";
  if (m.includes("permission") || m.includes("доступ")) return "нет доступа к файлу или папке";
  return "не удалось преобразовать";
}
