import { open } from "@tauri-apps/plugin-dialog";
import { useState } from "react";
import { api } from "../lib/api";
import { bytes } from "../lib/format";
import { useStore } from "../lib/store";
import { checkForUpdate, useUpdate } from "../lib/update";
import type { FfmpegSource, Theme } from "../lib/types";
import { Advanced } from "./Advanced";
import { AboutDialog } from "./AboutDialog";
import { LoadSettings } from "./LoadMeter";
import { Button, Dialog, Field, Select, Switch } from "./ui";

const sourceKey = (s: FfmpegSource) => (typeof s === "object" ? "Custom" : s);
const CODEC: Record<string, string> = { H264: "H.264", Hevc: "HEVC", Hevc10: "HEVC 10 бит", Av1: "AV1" };

/** Проверка обновлений: при запуске и по кнопке. Итог — словами, без всплывающих подсказок. */
function UpdateSettings() {
  const settings = useStore((s) => s.settings);
  const updateSettings = useStore((s) => s.updateSettings);
  const version = useStore((s) => s.info?.version);
  const { checkResult, info } = useUpdate();
  if (!settings) return null;
  const result =
    checkResult === "checking"
      ? "Проверяю…"
      : checkResult === "latest"
        ? `У вас последняя версия (${version}).`
        : checkResult === "failed"
          ? "Проверить не получилось. Возможно, нет подключения к интернету."
          : info
            ? `Вышла новая версия ${info.version} — кнопка «Обновить» вверху окна.`
            : null;
  return (
    <div className="space-y-3">
      <Switch
        checked={settings.check_updates ?? true}
        onChange={(check_updates) => updateSettings((s) => ({ ...s, check_updates }))}
        label="Проверять обновления при запуске"
        hint="Программа сама скажет, когда выйдет новая версия"
      />
      <div className="flex flex-wrap items-center gap-3">
        <Button disabled={checkResult === "checking"} onClick={() => void checkForUpdate(true)}>
          Проверить обновления сейчас
        </Button>
        {result && <span className="text-[16px] text-muted">{result}</span>}
      </div>
    </div>
  );
}

export function SettingsDialog({ open: isOpen, onClose }: { open: boolean; onClose: () => void }) {
  const settings = useStore((s) => s.settings);
  const info = useStore((s) => s.info);
  const updateSettings = useStore((s) => s.updateSettings);
  const setInfo = useStore((s) => s.setInfo);
  const runActive = useStore((s) => s.run.active);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [about, setAbout] = useState(false);
  if (!settings || !info) return null;

  const backends = [...new Set(info.hw.map((h) => h.backend))];
  const changed = [
    !settings.limit_on_battery && "нагрузка от батареи",
    !settings.convert.skip_live_photo_video && "видео Live Photo",
    settings.convert.remove_live_photo_video && "удаление видео Live Photo",
    settings.ffmpeg !== "Auto" && "источник ffmpeg",
    settings.load !== "Auto" && settings.load !== "Max" && "нагрузка",
  ].filter((x): x is string => Boolean(x));

  const recheck = async () => {
    setBusy(true);
    setNote(null);
    try {
      setInfo(await api.refreshHw());
      setNote("Видеокарта проверена заново.");
    } catch (e) {
      setNote(String(e));
    } finally {
      setBusy(false);
    }
  };

  const setSource = async (key: string) => {
    let ffmpeg: FfmpegSource = key as FfmpegSource;
    if (key === "Custom") {
      const picked = await open({ directory: true, title: "Папка с ffmpeg и ffprobe" });
      if (typeof picked !== "string") return;
      ffmpeg = { Custom: picked };
    }
    const next = { ...settings, ffmpeg };
    updateSettings(() => next);
    await api.saveSettings(next);
    setInfo(await api.appInfo());
  };

  return (
    <Dialog open={isOpen} onOpenChange={(v) => !v && onClose()} title="Настройки" width={560}>
      <div className="space-y-6">
        <section className="space-y-3">
          <h3 className="text-[18px] font-semibold">Основные</h3>
          <Field label="Тема">
            <Select<Theme>
              value={settings.theme}
              onChange={(theme) => updateSettings((s) => ({ ...s, theme }))}
              options={[
                { value: "System", label: "Как в системе" },
                { value: "Light", label: "Светлая" },
                { value: "Dark", label: "Тёмная" },
              ]}
            />
          </Field>
          <Switch
            checked={settings.large_text}
            onChange={(large_text) => updateSettings((s) => ({ ...s, large_text }))}
            label="Крупный текст"
            hint="Всё в окне станет крупнее"
          />
          <Switch
            checked={settings.prevent_sleep}
            onChange={(prevent_sleep) => updateSettings((s) => ({ ...s, prevent_sleep }))}
            label="Не давать компьютеру уснуть"
            hint="Пока программа работает с файлами"
          />
          <UpdateSettings />
        </section>

        <div className="-mx-5 border-t border-line">
          <Advanced id="settings" changed={changed} inset="px-5">
            <div className="space-y-6 px-5 pt-1 pb-2">
              <section className="space-y-3">
                <h3 className="text-[18px] font-semibold">Нагрузка</h3>
                <LoadSettings />
              </section>

              <section className="space-y-3">
                <h3 className="text-[18px] font-semibold">Во время работы</h3>
                <Switch
                  checked={settings.limit_on_battery}
                  onChange={(limit_on_battery) => updateSettings((s) => ({ ...s, limit_on_battery }))}
                  label="От батареи — не больше средней нагрузки"
                />
                <Switch
                  checked={settings.convert.skip_live_photo_video}
                  onChange={(v) => updateSettings((s) => ({ ...s, convert: { ...s.convert, skip_live_photo_video: v } }))}
                  label="Пропускать видео Live Photo"
                  hint="Короткие ролики рядом с фото iPhone с тем же именем"
                />
                <Switch
                  checked={settings.convert.remove_live_photo_video}
                  disabled={!settings.convert.skip_live_photo_video}
                  onChange={(v) => updateSettings((s) => ({ ...s, convert: { ...s.convert, remove_live_photo_video: v } }))}
                  label="Удалять видео Live Photo вместе с фото"
                  hint="Только если исходники отправляются в корзину или удаляются"
                />
              </section>

              <section className="space-y-3">
                <h3 className="text-[18px] font-semibold">ffmpeg</h3>
                <Field
                  label="Откуда брать"
                  hint={
                    info.ffmpeg ? (
                      <span className="block break-all selectable">
                        {info.ffmpeg.version.replace(/ Copyright.*$/, "")}
                        <br />
                        {info.ffmpeg.path}
                      </span>
                    ) : (
                      <span className="text-err">{info.ffmpeg_error}</span>
                    )
                  }
                >
                  <Select
                    value={sourceKey(settings.ffmpeg)}
                    disabled={runActive}
                    onChange={(v) => void setSource(v)}
                    options={[
                      { value: "Auto", label: "Автоматически", hint: "Windows — из комплекта, Linux — системный 6.1+" },
                      { value: "Bundled", label: "Из комплекта программы" },
                      { value: "System", label: "Системный (PATH)" },
                      { value: "Custom", label: "Указать папку…" },
                    ]}
                  />
                </Field>
              </section>

              <section className="space-y-3">
                <h3 className="text-[18px] font-semibold">Видеокарта</h3>
                {backends.length ? (
                  <ul className="space-y-1 text-[14px]">
                    {backends.map((b) => (
                      <li key={b} className="flex gap-3">
                        <span className="w-16 font-medium">{b}</span>
                        <span className="text-muted">
                          {info.hw
                            .filter((h) => h.backend === b)
                            .map((h) => CODEC[h.codec] ?? h.codec)
                            .join(", ")}
                        </span>
                      </li>
                    ))}
                    <li className="text-muted">Одновременно на видеокарте: до {info.max_sessions} видео</li>
                  </ul>
                ) : (
                  <p className="text-[14px] text-muted">Аппаратные кодировщики не найдены, видео кодирует процессор.</p>
                )}
                <div className="flex items-center gap-3">
                  <Button onClick={() => void recheck()} disabled={busy || runActive}>
                    {busy ? "Проверяю…" : "Проверить заново"}
                  </Button>
                  <span className="text-[14px] text-muted">После обновления драйвера</span>
                </div>
              </section>

              <section className="space-y-3">
                <h3 className="text-[18px] font-semibold">Хранение</h3>
                <div className="flex items-center gap-3">
                  <Button
                   
                    onClick={async () => setNote(`Освобождено ${bytes(await api.clearThumbCache())}.`)}
                  >
                    Очистить кэш миниатюр
                  </Button>
                </div>
                <p className="text-[14px] break-all text-muted selectable">Настройки: {info.settings_path}</p>
              </section>
            </div>
          </Advanced>
        </div>

        {note && <p className="text-[14px] text-accent">{note}</p>}
        <div className="flex items-center gap-3">
          <Button onClick={() => setAbout(true)}>О программе</Button>
          <span className="text-[14px] text-muted">Converter {info.version}</span>
        </div>
        <AboutDialog open={about} onClose={() => setAbout(false)} />
      </div>
    </Dialog>
  );
}
