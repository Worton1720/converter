import { openPath, openUrl } from "@tauri-apps/plugin-opener";
import { useStore } from "../lib/store";
import { Button, Dialog } from "./ui";

// Сторонние компоненты в поставке (ТЗ 8.4). ffmpeg — отдельная программа, поэтому GPL
// не распространяется на само приложение, но лицензию и ссылки на исходники нужно приложить.
const COMPONENTS: { name: string; license: string; url: string; note?: string }[] = [
  {
    name: "FFmpeg",
    license: "GPL 3.0",
    url: "https://ffmpeg.org/download.html",
    note: "Статическая сборка BtbN с x264 и x265, запускается как отдельная программа",
  },
  { name: "Сборка FFmpeg (BtbN)", license: "GPL 3.0", url: "https://github.com/BtbN/FFmpeg-Builds" },
  { name: "libheif", license: "LGPL 3.0", url: "https://github.com/strukturag/libheif" },
  { name: "libde265", license: "LGPL 3.0", url: "https://github.com/strukturag/libde265" },
  { name: "libwebp", license: "BSD", url: "https://chromium.googlesource.com/webm/libwebp" },
  { name: "Little CMS", license: "MIT", url: "https://www.littlecms.com" },
  { name: "Tauri", license: "MIT / Apache 2.0", url: "https://tauri.app" },
  { name: "React", license: "MIT", url: "https://react.dev" },
  { name: "Шрифт Onest", license: "SIL Open Font License 1.1", url: "https://github.com/simpletype-foundry/Onest" },
  { name: "Значки Lucide", license: "ISC", url: "https://lucide.dev" },
];

export function AboutDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const info = useStore((s) => s.info);
  const bundledLicense =
    info?.ffmpeg?.origin === "Bundled" ? info.ffmpeg.path.replace(/ffmpeg(\.exe)?$/, "LICENSE.txt") : null;
  return (
    <Dialog
      open={open}
      onOpenChange={(v) => !v && onClose()}
      title="О программе"
      width={620}
      footer={
        <Button variant="primary" onClick={onClose}>
          Закрыть
        </Button>
      }
    >
      <div className="space-y-5">
        <div className="space-y-1">
          <p className="text-[18px] font-semibold">Converter {info?.version}</p>
          <p className="text-muted">
            Переводит фото и видео с телефона в форматы, которые открываются везде, и находит одинаковые файлы.
            Распространяется бесплатно по лицензии MIT.
          </p>
        </div>
        <div className="space-y-2">
          <h3 className="text-[18px] font-semibold">Что входит в программу</h3>
          <ul className="divide-y divide-line">
            {COMPONENTS.map((c) => (
              <li key={c.name} className="flex items-center gap-3 py-2">
                <span className="min-w-0 flex-1">
                  <span className="block font-medium">{c.name}</span>
                  <span className="block text-[14px] text-muted">
                    {c.license}
                    {c.note && `. ${c.note}`}
                  </span>
                </span>
                <Button variant="ghost" onClick={() => void openUrl(c.url)}>
                  Исходники
                </Button>
              </li>
            ))}
          </ul>
          {bundledLicense && (
            <Button onClick={() => void openPath(bundledLicense)}>Открыть лицензию FFmpeg</Button>
          )}
        </div>
      </div>
    </Dialog>
  );
}
