import clsx from "clsx";
import { ChevronRight } from "lucide-react";
import { useState, type ReactNode } from "react";

function readOpen(id: string): boolean {
  try {
    return localStorage.getItem(`converter.advanced.${id}`) === "1";
  } catch {
    return false;
  }
}

function writeOpen(id: string, open: boolean) {
  try {
    localStorage.setItem(`converter.advanced.${id}`, open ? "1" : "0");
  } catch {
    // Без хранилища блок просто снова будет свёрнут.
  }
}

/** Свёрнутый блок для редких настроек. Когда он закрыт, видно, что внутри изменено. */
export function Advanced({
  id,
  changed,
  children,
  className,
  inset = "px-4",
}: {
  id: string;
  /** Названия изменённых настроек — показываются, пока блок свёрнут. */
  changed: string[];
  children: ReactNode;
  className?: string;
  /** Горизонтальный отступ заголовка — как у содержимого вокруг. */
  inset?: string;
}) {
  const [open, setOpen] = useState(() => readOpen(id));
  const toggle = () => {
    setOpen(!open);
    writeOpen(id, !open);
  };
  return (
    <div className={className}>
      <button
        type="button"
        onClick={toggle}
        aria-expanded={open}
        className={clsx("flex w-full items-start gap-1.5 py-3 text-left hover:bg-sunken/60", inset)}
      >
        <ChevronRight className={clsx("mt-0.5 size-5 shrink-0 text-muted transition-transform", open && "rotate-90")} />
        <span className="min-w-0">
          <span className="block text-[16px] font-medium">Расширенные настройки</span>
          {!open && (
            <span className="block text-[14px] leading-snug text-muted">
              {changed.length ? `Изменено: ${changed.join(", ")}` : "Обычно менять не нужно"}
            </span>
          )}
        </span>
      </button>
      {open && children}
    </div>
  );
}
