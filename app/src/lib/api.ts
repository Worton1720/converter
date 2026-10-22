import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import type {
  ActionResult,
  AppInfo,
  ConvertOptions,
  DupAction,
  DupesOptions,
  DupesResult,
  Item,
  KeepRule,
  LoadLevel,
  PhotoKind,
  HistoryItem,
  UndoReport,
  UpdateInfo,
  Settings,
  SpaceCheck,
  SpaceEstimate,
} from "./types";

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),
  refreshHw: () => invoke<AppInfo>("refresh_hw"),
  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
  setLoad: (level: LoadLevel) => invoke<void>("set_load", { level }),

  addPaths: (paths: string[]) => invoke<number>("add_paths", { paths }),
  listItems: () => invoke<Item[]>("list_items"),
  removeItems: (paths: string[]) => invoke<void>("remove_items", { paths }),
  clearItems: () => invoke<void>("clear_items"),

  spaceEstimate: (options: ConvertOptions, paths: string[]) =>
    invoke<SpaceEstimate>("space_estimate", { options, paths }),
  spaceCheck: (options: ConvertOptions, paths: string[]) =>
    invoke<SpaceCheck[]>("space_check", { options, paths }),
  startConvert: (options: ConvertOptions, paths: string[]) =>
    invoke<void>("start_convert", { options, paths }),
  pauseConvert: (paused: boolean) => invoke<void>("pause_convert", { paused }),
  cancelConvert: () => invoke<void>("cancel_convert"),
  cancelItem: (path: string) => invoke<void>("cancel_item", { path }),

  historyList: () => invoke<HistoryItem[]>("history_list"),
  historyUndo: (id: number) => invoke<UndoReport>("history_undo", { id }),
  checkUpdate: () => invoke<UpdateInfo | null>("check_update"),
  installUpdate: () => invoke<void>("install_update"),
  dupesFind: (options: DupesOptions) => invoke<DupesResult>("dupes_find", { options }),
  dupesCancel: () => invoke<void>("dupes_cancel"),
  dupesAuto: (rule: KeepRule, keepFormat: PhotoKind) =>
    invoke<boolean[][]>("dupes_auto", { rule, keepFormat }),
  dupesApply: (marks: boolean[][], action: DupAction) =>
    invoke<ActionResult[]>("dupes_apply", { marks, action }),
  dupesExportCsv: (path: string, marks: boolean[][]) =>
    invoke<void>("dupes_export_csv", { path, marks }),

  clearThumbCache: () => invoke<number>("clear_thumb_cache"),
  writeTextFile: (path: string, contents: string) => invoke<void>("write_text_file", { path, contents }),
  quit: () => invoke<void>("quit"),
};

/** URL миниатюры через протокол `thumb` (см. lib.rs). */
export function thumbUrl(path: string, size?: number): string {
  const url = convertFileSrc(path, "thumb");
  return size ? `${url}?s=${size}` : url;
}
