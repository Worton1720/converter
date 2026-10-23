import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";
import { api } from "./api";
import { useStore } from "./store";
import type { UpdateInfo } from "./types";

/** Состояние автообновления — отдельно от основного хранилища: оно живёт поверх всех вкладок. */
interface UpdateState {
  info: UpdateInfo | null;
  /** Ручная проверка из настроек: итог показывается словами. */
  checkResult: "checking" | "latest" | "failed" | null;
  /** Доля загрузки 0–1; `null` — не загружается. */
  progress: number | null;
  error: string | null;
  /** «Не сейчас» — до следующего запуска. */
  dismissed: boolean;
}

export const useUpdate = create<UpdateState>(() => ({
  info: null,
  checkResult: null,
  progress: null,
  error: null,
  dismissed: false,
}));

let startupChecked = false;

/** Проверка обновления. При запуске (`manual = false`) ошибки сети не показываются. */
export async function checkForUpdate(manual: boolean) {
  // Эффекты в режиме разработки срабатывают дважды — при запуске проверяем один раз.
  if (!manual) {
    if (startupChecked) return;
    startupChecked = true;
  }
  if (manual) useUpdate.setState({ checkResult: "checking", dismissed: false });
  try {
    const info = await api.checkUpdate();
    useUpdate.setState({ info, checkResult: info ? null : "latest", error: null });
  } catch (e) {
    useStore.getState().addLog("Warn", `Проверка обновлений: ${e}`);
    if (manual) useUpdate.setState({ checkResult: "failed" });
  }
}

export async function installUpdate() {
  useUpdate.setState({ progress: 0, error: null });
  const un = await listen<number>("update:progress", (e) => useUpdate.setState({ progress: e.payload }));
  try {
    // После установки программа перезапускается сама — до этой строки обычно не доходит.
    await api.installUpdate();
  } catch (e) {
    useStore.getState().addLog("Error", `Обновление: ${e}`);
    useUpdate.setState({ progress: null, error: String(e) });
  } finally {
    un();
  }
}
