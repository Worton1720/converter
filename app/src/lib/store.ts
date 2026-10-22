import { create } from "zustand";
import { api } from "./api";
import { basename } from "./format";
import type {
  AppInfo,
  ConvertOptions,
  DupesProgress,
  DupesResult,
  EngineEvent,
  GovernorStatus,
  Item,
  JobOutcome,
  RunSummary,
  Settings,
  SourceAction,
} from "./types";

export type Tab = "convert" | "dupes" | "history" | "log";
export type JobStatus = "waiting" | "running" | "done" | "skipped" | "failed" | "cancelled";

export interface JobState {
  status: JobStatus;
  progress: number;
  speed: number | null;
  operation?: string;
  output?: string;
  message?: string;
  source?: SourceAction;
}

export interface Row {
  item: Item;
  job: JobState;
}

export interface LogEntry {
  n: number;
  time: number;
  level: "Info" | "Warn" | "Error";
  path?: string;
  message: string;
}

export interface RunState {
  active: boolean;
  paused: boolean;
  noSpace: string | null;
  startedAt: number;
  paths: string[];
  finished: number;
  summary: RunSummary | null;
}

export interface DupesState {
  roots: string[];
  running: boolean;
  progress: DupesProgress | null;
  result: DupesResult | null;
  marks: boolean[][];
  focus: { g: number; f: number } | null;
  notice: string | null;
}

interface Store {
  tab: Tab;
  info: AppInfo | null;
  settings: Settings | null;
  status: GovernorStatus | null;
  rows: Record<string, Row>;
  order: string[];
  selected: Set<string>;
  run: RunState;
  jobPaths: Record<number, string>;
  log: LogEntry[];
  dupes: DupesState;
  closeRequested: boolean;

  setTab: (tab: Tab) => void;
  setInfo: (info: AppInfo) => void;
  setStatus: (s: GovernorStatus) => void;
  updateSettings: (fn: (s: Settings) => Settings) => void;
  updateConvert: (fn: (c: ConvertOptions) => ConvertOptions) => void;
  addItems: (items: Item[]) => void;
  replaceItems: (items: Item[]) => void;
  removePaths: (paths: string[]) => void;
  clearItems: () => void;
  select: (paths: string[], mode: "set" | "toggle" | "add") => void;
  startRun: (paths: string[]) => void;
  applyEvents: (events: EngineEvent[]) => void;
  setPaused: (paused: boolean) => void;
  addLog: (level: LogEntry["level"], message: string, path?: string) => void;
  clearLog: () => void;
  setDupes: (fn: (d: DupesState) => DupesState) => void;
  setCloseRequested: (v: boolean) => void;
}

const idle: JobState = { status: "waiting", progress: 0, speed: null };

let saveTimer: ReturnType<typeof setTimeout> | undefined;
function scheduleSave(settings: Settings) {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    api.saveSettings(settings).catch((e) => useStore.getState().addLog("Error", `Настройки не сохранены: ${e}`));
  }, 400);
}

function outcomeToJob(prev: JobState, outcome: JobOutcome): JobState {
  if (outcome === "Cancelled") return { ...prev, status: "cancelled", speed: null };
  if ("Done" in outcome) {
    return {
      ...prev,
      status: "done",
      progress: 1,
      speed: null,
      output: outcome.Done.output,
      operation: outcome.Done.operation,
      source: outcome.Done.source,
    };
  }
  if ("Skipped" in outcome) return { ...prev, status: "skipped", speed: null, message: outcome.Skipped.reason };
  return { ...prev, status: "failed", speed: null, message: outcome.Failed.error };
}

let logCounter = 0;

export const useStore = create<Store>((set, get) => ({
  tab: "convert",
  info: null,
  settings: null,
  status: null,
  rows: {},
  order: [],
  selected: new Set(),
  run: { active: false, paused: false, noSpace: null, startedAt: 0, paths: [], finished: 0, summary: null },
  jobPaths: {},
  log: [],
  dupes: { roots: [], running: false, progress: null, result: null, marks: [], focus: null, notice: null },
  closeRequested: false,

  setTab: (tab) => set({ tab }),
  setInfo: (info) => set({ info, settings: info.settings, status: info.status ?? get().status }),
  setStatus: (status) => set({ status }),

  updateSettings: (fn) => {
    const current = get().settings;
    if (!current) return;
    const next = fn(current);
    set({ settings: next });
    scheduleSave(next);
  },
  updateConvert: (fn) => get().updateSettings((s) => ({ ...s, convert: fn(s.convert) })),

  addItems: (items) =>
    set((st) => {
      const rows = { ...st.rows };
      const order = [...st.order];
      for (const item of items) {
        if (!rows[item.path]) order.push(item.path);
        rows[item.path] = { item, job: rows[item.path]?.job ?? idle };
      }
      return { rows, order };
    }),

  replaceItems: (items) =>
    set((st) => {
      const rows = { ...st.rows };
      for (const item of items) {
        if (rows[item.path]) rows[item.path] = { ...rows[item.path], item };
      }
      return { rows };
    }),

  removePaths: (paths) => {
    const drop = new Set(paths);
    set((st) => {
      const rows = { ...st.rows };
      for (const p of paths) delete rows[p];
      const selected = new Set([...st.selected].filter((p) => !drop.has(p)));
      return { rows, order: st.order.filter((p) => !drop.has(p)), selected };
    });
    void api.removeItems(paths);
  },

  clearItems: () => {
    set({ rows: {}, order: [], selected: new Set() });
    void api.clearItems();
  },

  select: (paths, mode) =>
    set((st) => {
      if (mode === "set") return { selected: new Set(paths) };
      const selected = new Set(st.selected);
      for (const p of paths) {
        if (mode === "toggle" && selected.has(p)) selected.delete(p);
        else selected.add(p);
      }
      return { selected };
    }),

  startRun: (paths) =>
    set((st) => {
      const rows = { ...st.rows };
      for (const p of paths) {
        if (rows[p]) rows[p] = { ...rows[p], job: { ...idle } };
      }
      const jobPaths: Record<number, string> = {};
      paths.forEach((p, i) => (jobPaths[i + 1] = p));
      return {
        rows,
        jobPaths,
        run: { active: true, paused: false, noSpace: null, startedAt: Date.now(), paths, finished: 0, summary: null },
      };
    }),

  applyEvents: (events) =>
    set((st) => {
      const rows = { ...st.rows };
      const run = { ...st.run };
      const log = [...st.log];
      let status = st.status;
      const pathOf = (id: number | null) => (id == null ? undefined : st.jobPaths[id]);
      const patch = (path: string | undefined, fn: (j: JobState) => JobState) => {
        if (path && rows[path]) rows[path] = { ...rows[path], job: fn(rows[path].job) };
      };
      const push = (level: LogEntry["level"], message: string, path?: string) =>
        log.push({ n: ++logCounter, time: Date.now(), level, path, message });

      for (const e of events) {
        if ("JobStarted" in e) {
          const { id, operation } = e.JobStarted;
          patch(pathOf(id), (j) => ({ ...j, status: "running", operation, progress: 0 }));
        } else if ("JobProgress" in e) {
          const { id, fraction, speed } = e.JobProgress;
          patch(pathOf(id), (j) => ({ ...j, progress: fraction, speed }));
        } else if ("JobFinished" in e) {
          const { source, outcome } = e.JobFinished;
          patch(source, (j) => outcomeToJob(j, outcome));
          run.finished += 1;
          const name = basename(source);
          if (outcome === "Cancelled") push("Info", `${name}: отменено`, source);
          else if ("Done" in outcome) push("Info", `${name} → ${basename(outcome.Done.output)} (${outcome.Done.operation})`, source);
          else if ("Skipped" in outcome) push("Info", `${name}: пропущено, ${outcome.Skipped.reason}`, source);
          else push("Error", `${name}: ${outcome.Failed.error}`, source);
          if (typeof outcome !== "string" && "Done" in outcome && typeof outcome.Done.source === "object") {
            push("Warn", `${name}: исходник оставлен — ${outcome.Done.source.KeptBecause}`, source);
          }
        } else if ("Log" in e) {
          push(e.Log.level, e.Log.message, pathOf(e.Log.id));
        } else if ("PausedNoSpace" in e) {
          run.paused = true;
          run.noSpace = e.PausedNoSpace.path;
          push("Warn", `Кончилось место на диске: ${e.PausedNoSpace.path}. Очередь на паузе.`);
        } else if ("Governor" in e) {
          status = e.Governor;
        } else if ("RunFinished" in e) {
          run.active = false;
          run.paused = false;
          run.summary = e.RunFinished;
          const s = e.RunFinished;
          push(
            s.failed > 0 ? "Warn" : "Info",
            `Готово: ${s.done} успешно, ${s.skipped} пропущено, ${s.failed} с ошибкой, ${s.cancelled} отменено.`,
          );
        }
      }
      return { rows, run, log: log.length > 5000 ? log.slice(-5000) : log, status };
    }),

  setPaused: (paused) => set((st) => ({ run: { ...st.run, paused, noSpace: paused ? st.run.noSpace : null } })),

  addLog: (level, message, path) =>
    set((st) => ({ log: [...st.log, { n: ++logCounter, time: Date.now(), level, path, message }] })),
  clearLog: () => set({ log: [] }),

  setDupes: (fn) => set((st) => ({ dupes: fn(st.dupes) })),
  setCloseRequested: (closeRequested) => set({ closeRequested }),
}));

export function convertOptions(): ConvertOptions | null {
  return useStore.getState().settings?.convert ?? null;
}
