import { openUrl } from "@tauri-apps/plugin-opener";
import { useState } from "react";
import { useStore } from "../lib/store";
import { installUpdate, useUpdate } from "../lib/update";
import { Button, Dialog, ProgressBar } from "./ui";

const DOWNLOAD_PAGE = "https://github.com/Worton1720/converter/releases/latest";

/** Раздел выпуска из CHANGELOG — заголовки и пункты, без разметки. */
function Notes({ text }: { text: string }) {
  return (
    <div className="space-y-2 text-[16px] leading-relaxed">
      {text
        .split("\n")
        .map((line) => line.replace(/`/g, "").replace(/\*\*/g, ""))
        .filter((line) => line.trim())
        .map((line, i) =>
          line.startsWith("#") ? (
            <h4 key={i} className="pt-2 text-[18px] font-semibold">
              {line.replace(/^#+\s*/, "")}
            </h4>
          ) : (
            <p key={i} className={line.startsWith("- ") ? "pl-5 -indent-4" : undefined}>
              {line.startsWith("- ") ? `• ${line.slice(2)}` : line}
            </p>
          ),
        )}
    </div>
  );
}

/** Полоса «Вышла новая версия» над рабочей областью: одна главная кнопка, понятные слова. */
export function UpdateBanner() {
  const { info, progress, error, dismissed } = useUpdate();
  const busy = useStore((s) => s.run.active);
  const [notes, setNotes] = useState(false);
  if (!info || (dismissed && progress === null)) return null;

  const selfUpdate = info.installer !== null;
  return (
    <div className="shrink-0 border-b border-line bg-accent-soft px-6 py-3">
      {progress !== null ? (
        <div className="flex flex-wrap items-center gap-4">
          <span className="text-[16px]">
            {progress < 1
              ? `Загружаю новую версию: ${Math.round(progress * 100)} %`
              : "Устанавливаю новую версию. Программа перезапустится сама."}
          </span>
          <div className="w-64">
            <ProgressBar value={progress} />
          </div>
        </div>
      ) : (
        <div className="flex flex-wrap items-center gap-3">
          <div className="min-w-0 flex-1 text-[16px] leading-relaxed">
            {error ? (
              <>Обновить не получилось. Новую версию можно скачать вручную со страницы загрузки.</>
            ) : (
              <>
                <span className="font-semibold">Вышла новая версия программы ({info.version}).</span>{" "}
                {selfUpdate
                  ? "Обновление займёт пару минут, ваши файлы и настройки останутся."
                  : "Скачайте её со страницы загрузки и замените старую."}
                {info.installer === "deb" && " Компьютер может спросить пароль — это нормально."}
                {busy && selfUpdate && " Обновить можно, когда программа закончит работу."}
              </>
            )}
          </div>
          {selfUpdate && !error ? (
            <Button variant="primary" disabled={busy} onClick={() => void installUpdate()}>
              Обновить
            </Button>
          ) : (
            <Button variant="primary" onClick={() => void openUrl(DOWNLOAD_PAGE)}>
              Открыть страницу загрузки
            </Button>
          )}
          {info.notes && <Button onClick={() => setNotes(true)}>Что нового</Button>}
          <Button variant="ghost" onClick={() => useUpdate.setState({ dismissed: true })}>
            Не сейчас
          </Button>
        </div>
      )}
      <Dialog
        open={notes}
        onOpenChange={setNotes}
        title={`Что нового в версии ${info.version}`}
        width={620}
        footer={
          <Button variant="primary" onClick={() => setNotes(false)}>
            Закрыть
          </Button>
        }
      >
        <Notes text={info.notes ?? ""} />
      </Dialog>
    </div>
  );
}
