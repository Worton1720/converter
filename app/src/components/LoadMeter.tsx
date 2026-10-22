import clsx from "clsx";
import { useState } from "react";
import { api } from "../lib/api";
import { plural } from "../lib/format";
import { useStore } from "../lib/store";
import type { LoadLevel } from "../lib/types";
import { Button, Dialog, Field, Select, Slider, Switch } from "./ui";

type LevelKey = "Auto" | "Low" | "Medium" | "High" | "Max" | "Custom";

const LEVELS: { value: LevelKey; label: string; hint: string }[] = [
  { value: "Auto", label: "Авто", hint: "Уступает процессор другим программам" },
  { value: "Low", label: "Низкая", hint: "Фоном, четверть процессора" },
  { value: "Medium", label: "Средняя", hint: "Половина процессора" },
  { value: "High", label: "Высокая", hint: "Три четверти процессора" },
  { value: "Max", label: "Максимальная", hint: "Весь процессор" },
  { value: "Custom", label: "Своя…", hint: "Точное число потоков" },
];

const levelKey = (l: LoadLevel): LevelKey => (typeof l === "object" ? "Custom" : l);

/** Нагрузка на компьютер для опытных: уровни и ячейки — по одной на поток процессора. */
export function LoadSettings() {
  const status = useStore((s) => s.status);
  const settings = useStore((s) => s.settings);
  const info = useStore((s) => s.info);
  const updateSettings = useStore((s) => s.updateSettings);
  const [customOpen, setCustomOpen] = useState(false);

  const total = status?.threads_total ?? info?.threads ?? 0;
  const budget = status?.budget ?? 0;
  const level = settings?.load ?? "Auto";
  const otherCells = Math.min(Math.round((status?.external_load ?? 0) * total), Math.max(0, total - budget));

  const choose = (next: LoadLevel) => {
    updateSettings((s) => ({ ...s, load: next }));
    void api.setLoad(next);
  };

  return (
    <div className="space-y-3">
      <Field label="Нагрузка на компьютер">
        <Select<LevelKey>
          value={levelKey(level)}
          onChange={(k) => (k === "Custom" ? setCustomOpen(true) : choose(k))}
          options={LEVELS}
        />
      </Field>
      <div className="flex items-center gap-3" role="img" aria-label={`Доступно ${budget} из ${total} потоков`}>
        <div className="flex gap-1">
          {Array.from({ length: total }, (_, i) => {
            const ours = i < budget;
            const other = !ours && i >= total - otherCells;
            return (
              <span
                key={i}
                className={clsx(
                  "h-6 w-3 rounded-[3px] transition-colors duration-500",
                  ours && "bg-accent",
                  other && "bg-cell-other",
                  !ours && !other && "border-2 border-line-strong",
                )}
              />
            );
          })}
        </div>
        <span className="text-[16px] tabular-nums">
          {budget} из {total}
        </span>
      </div>
      {status && (
        <p className="text-[14px] leading-snug text-muted">
          Программе сейчас доступно {budget} {plural(budget, "поток", "потока", "потоков")} процессора из {total}. Другие
          программы заняли {Math.round(status.external_load * 100)} %.
          {status.on_battery && " Компьютер работает от батареи, поэтому нагрузка не выше средней."}
          {status.memory_low && " Мало свободной памяти — новые файлы ждут своей очереди."}
        </p>
      )}
      {customOpen && (
        <CustomLevelDialog
          total={total}
          initial={typeof level === "object" ? level.Custom : null}
          onClose={() => setCustomOpen(false)}
          onApply={(c) => {
            choose({ Custom: c });
            setCustomOpen(false);
          }}
        />
      )}
    </div>
  );
}

function CustomLevelDialog({
  total,
  initial,
  onClose,
  onApply,
}: {
  total: number;
  initial: { threads: number; low_priority: boolean; hw_sessions: number } | null;
  onClose: () => void;
  onApply: (c: { threads: number; low_priority: boolean; hw_sessions: number }) => void;
}) {
  const [threads, setThreads] = useState(initial?.threads ?? Math.max(1, Math.ceil(total / 2)));
  const [low, setLow] = useState(initial?.low_priority ?? false);
  const [hw, setHw] = useState(initial?.hw_sessions ?? 1);
  return (
    <Dialog
      open
      onOpenChange={(v) => !v && onClose()}
      title="Своя нагрузка"
      width={480}
      footer={
        <>
          <Button onClick={onClose}>Отмена</Button>
          <Button variant="primary" onClick={() => onApply({ threads, low_priority: low, hw_sessions: hw })}>
            Применить
          </Button>
        </>
      }
    >
      <div className="space-y-5">
        <Field label={`Потоки процессора: ${threads} из ${total}`}>
          <Slider value={threads} min={1} max={Math.max(1, total)} onChange={setThreads} label="Потоки процессора" />
        </Field>
        <Field label={`Видео одновременно на видеокарте: ${hw}`}>
          <Slider value={hw} min={1} max={3} onChange={setHw} label="Сессии видеокарты" />
        </Field>
        <Switch
          checked={low}
          onChange={setLow}
          label="Низкий приоритет"
          hint="Другие программы получают процессор первыми, даже если заняли не всё."
        />
      </div>
    </Dialog>
  );
}
