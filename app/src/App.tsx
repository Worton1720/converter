import * as TooltipPrimitive from "@radix-ui/react-tooltip";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useEffect, useState } from "react";
import { Header } from "./components/Header";
import { SettingsDialog } from "./components/SettingsDialog";
import { UpdateBanner } from "./components/UpdateBanner";
import { Button, Dialog } from "./components/ui";
import { api } from "./lib/api";
import { useStore } from "./lib/store";
import { checkForUpdate } from "./lib/update";
import type { EngineEvent, GovernorStatus, Item } from "./lib/types";
import { addPaths, ConvertView } from "./views/convert/ConvertView";
import { DupesView } from "./views/dupes/DupesView";
import { HistoryView } from "./views/HistoryView";
import { LogView } from "./views/LogView";

function useTheme() {
  const theme = useStore((s) => s.settings?.theme ?? "System");
  const large = useStore((s) => s.settings?.large_text ?? false);
  useEffect(() => {
    // Масштаб всего окна, включая диалоги (ТЗ 8.5 «Крупный текст»).
    document.documentElement.style.setProperty("zoom", large ? "1.2" : "");
  }, [large]);
  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      const dark = theme === "Dark" || (theme === "System" && media.matches);
      document.documentElement.dataset.theme = dark ? "dark" : "light";
    };
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [theme]);
}

/** Куда отправить пути из перетаскивания или второго запуска программы. */
function routePaths(paths: string[]) {
  const { tab, setDupes } = useStore.getState();
  if (tab === "dupes") setDupes((d) => ({ ...d, roots: [...new Set([...d.roots, ...paths])] }));
  else void addPaths(paths);
}

function useBackendEvents() {
  useEffect(() => {
    const s = useStore.getState();
    const subs = [
      listen<Item[]>("items:added", (e) => useStore.getState().addItems(e.payload)),
      listen<Item[]>("items:updated", (e) => useStore.getState().replaceItems(e.payload)),
      listen<EngineEvent[]>("convert:events", (e) => useStore.getState().applyEvents(e.payload)),
      listen<GovernorStatus>("governor", (e) => useStore.getState().setStatus(e.payload)),
      listen<string[]>("open-paths", (e) => routePaths(e.payload)),
      listen("close-requested", () => useStore.getState().setCloseRequested(true)),
      getCurrentWebview().onDragDropEvent((e) => {
        if (e.payload.type === "drop") {
          routePaths(e.payload.paths);
        }
      }),
    ];
    api
      .appInfo()
      .then(async (info) => {
        s.setInfo(info);
        const existing = await api.listItems();
        if (existing.length) s.addItems(existing);
        if (info.ffmpeg_error) s.addLog("Error", info.ffmpeg_error);
        if (info.startup_paths.length) void addPaths(info.startup_paths);
        // Не сразу: запуск и первый список файлов важнее.
        if (info.settings.check_updates ?? true) setTimeout(() => void checkForUpdate(false), 5000);
      })
      .catch((e) => s.addLog("Error", `Не удалось запустить ядро: ${e}`));
    return () => subs.forEach((p) => void p.then((un) => un()));
  }, []);
}

function useDragOverlay() {
  const [over, setOver] = useState(false);
  useEffect(() => {
    const un = getCurrentWebview().onDragDropEvent((e) => {
      if (e.payload.type === "enter" || e.payload.type === "over") setOver(true);
      else setOver(false);
    });
    return () => void un.then((f) => f());
  }, []);
  return over;
}

export default function App() {
  const tab = useStore((s) => s.tab);
  const ready = useStore((s) => s.settings !== null);
  const closeRequested = useStore((s) => s.closeRequested);
  const setCloseRequested = useStore((s) => s.setCloseRequested);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const dragging = useDragOverlay();
  useTheme();
  useBackendEvents();

  return (
    <TooltipPrimitive.Provider>
      <div className="flex h-full flex-col">
        <Header onSettings={() => setSettingsOpen(true)} />
        <UpdateBanner />
        <main className="flex min-h-0 flex-1">
          {!ready ? null : tab === "convert" ? <ConvertView /> : tab === "dupes" ? (
            <DupesView />
          ) : tab === "history" ? (
            <HistoryView />
          ) : (
            <LogView />
          )}
        </main>
      </div>

      {dragging && (
        <div className="pointer-events-none fixed inset-2 z-30 flex items-center justify-center rounded-xl border-2 border-dashed border-accent bg-accent-soft/80">
          <span className="text-[20px] font-medium text-accent">
            {tab === "dupes" ? "Отпустите — поищем одинаковые файлы здесь" : "Отпустите — фото и видео добавятся в список"}
          </span>
        </div>
      )}

      <SettingsDialog open={settingsOpen} onClose={() => setSettingsOpen(false)} />

      <Dialog
        open={closeRequested}
        onOpenChange={setCloseRequested}
        title="Программа ещё работает"
        width={440}
        footer={
          <>
            <Button variant="primary" onClick={() => setCloseRequested(false)}>
              Продолжить работу
            </Button>
            <Button variant="danger" onClick={() => void api.quit()}>
              Остановить и закрыть
            </Button>
          </>
        }
      >
        <p className="text-[16px] leading-relaxed">
          Если закрыть программу сейчас, незаконченные файлы не сохранятся. Исходные файлы останутся на месте, а уже
          готовые результаты сохранятся.
        </p>
      </Dialog>
    </TooltipPrimitive.Provider>
  );
}
