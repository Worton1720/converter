import clsx from "clsx";
import { Settings2 } from "lucide-react";
import { api } from "../lib/api";
import { useStore, type Tab } from "../lib/store";
import type { LoadLevel } from "../lib/types";
import { IconButton, Select } from "./ui";

const TABS: { id: Tab; label: string }[] = [
  { id: "convert", label: "Преобразовать" },
  { id: "dupes", label: "Одинаковые файлы" },
  { id: "log", label: "Подробности" },
];

type Speed = "Auto" | "Max" | "Other";

/** Скорость работы двумя понятными вариантами; точные уровни — в настройках. */
function SpeedSelect() {
  const level = useStore((s) => s.settings?.load ?? "Auto");
  const updateSettings = useStore((s) => s.updateSettings);
  const value: Speed = level === "Auto" || level === "Max" ? level : "Other";
  const choose = (v: Speed) => {
    if (v === "Other") return;
    const next: LoadLevel = v;
    updateSettings((s) => ({ ...s, load: next }));
    void api.setLoad(next);
  };
  return (
    <div className="flex items-center gap-2">
      <span className="text-[16px] text-muted">Скорость работы:</span>
      <div className="w-64">
        <Select<Speed>
          ariaLabel="Скорость работы"
          value={value}
          onChange={choose}
          options={[
            { value: "Auto", label: "Не мешать моей работе", hint: "Компьютер остаётся отзывчивым" },
            { value: "Max", label: "Как можно быстрее", hint: "Когда компьютер не нужен" },
            ...(value === "Other" ? [{ value: "Other" as const, label: "Особая (в настройках)" }] : []),
          ]}
        />
      </div>
    </div>
  );
}

export function Header({ onSettings }: { onSettings: () => void }) {
  const tab = useStore((s) => s.tab);
  const setTab = useStore((s) => s.setTab);
  const errors = useStore((s) => s.log.filter((l) => l.level === "Error").length);

  return (
    <header className="flex h-16 shrink-0 items-stretch gap-6 border-b border-line bg-surface px-5">
      <div className="flex items-center text-[20px] font-semibold tracking-[-0.01em]">Converter</div>
      <nav className="flex items-stretch gap-1" role="tablist">
        {TABS.map((t) => (
          <button
            key={t.id}
            role="tab"
            aria-selected={tab === t.id}
            onClick={() => setTab(t.id)}
            className={clsx(
              "relative flex items-center gap-2 px-3 text-[18px] transition-colors",
              tab === t.id ? "font-medium text-text" : "text-muted hover:text-text",
            )}
          >
            {t.label}
            {t.id === "log" && errors > 0 && (
              <span className="rounded-full bg-err-soft px-2 text-[14px] font-medium text-err tabular-nums">
                {errors}
              </span>
            )}
            {tab === t.id && <span className="absolute inset-x-3 bottom-0 h-1 rounded-full bg-accent" />}
          </button>
        ))}
      </nav>
      <div className="ml-auto flex items-center gap-4">
        <SpeedSelect />
        <IconButton label="Настройки" onClick={onSettings}>
          <Settings2 className="size-5" />
        </IconButton>
      </div>
    </header>
  );
}
