import * as CheckboxPrimitive from "@radix-ui/react-checkbox";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import * as SelectPrimitive from "@radix-ui/react-select";
import * as SliderPrimitive from "@radix-ui/react-slider";
import * as SwitchPrimitive from "@radix-ui/react-switch";
import * as TooltipPrimitive from "@radix-ui/react-tooltip";
import clsx from "clsx";
import { Check, ChevronDown, X } from "lucide-react";
import type { ButtonHTMLAttributes, ReactNode } from "react";

// Размеры по ТЗ 8.5: всё, на что нажимают, — не меньше 44 × 44 px, текст — от 16 px.

type Variant = "primary" | "secondary" | "ghost" | "danger";

export function Button({
  variant = "secondary",
  size = "md",
  className,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant; size?: "md" | "lg" }) {
  return (
    <button
      {...props}
      className={clsx(
        "inline-flex shrink-0 items-center justify-center gap-2 rounded-lg font-medium whitespace-nowrap transition-colors",
        "disabled:cursor-not-allowed disabled:opacity-45",
        size === "md" ? "h-11 px-4 text-[16px]" : "h-14 px-6 text-[18px]",
        variant === "primary" && "bg-accent text-accent-ink hover:bg-accent-hover",
        variant === "secondary" && "border border-line-strong bg-surface text-text hover:bg-sunken",
        variant === "ghost" && "text-accent hover:bg-accent-soft",
        variant === "danger" && "bg-err text-white hover:opacity-90",
        className,
      )}
    />
  );
}

/** Кнопка со значком всегда подписана словами: значки сами по себе непонятны новичку. */
export function IconButton({
  label,
  className,
  children,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string }) {
  return (
    <button
      {...props}
      className={clsx(
        "inline-flex h-11 shrink-0 items-center gap-2 rounded-lg px-3 text-[16px] text-muted transition-colors hover:bg-sunken hover:text-text disabled:opacity-40",
        className,
      )}
    >
      {children}
      <span>{label}</span>
    </button>
  );
}

/** Подсказка — только дополнение: всё важное написано на экране. */
export function Tooltip({ content, children }: { content: ReactNode; children: ReactNode }) {
  if (!content) return <>{children}</>;
  return (
    <TooltipPrimitive.Root delayDuration={350}>
      <TooltipPrimitive.Trigger asChild>{children}</TooltipPrimitive.Trigger>
      <TooltipPrimitive.Portal>
        <TooltipPrimitive.Content
          sideOffset={6}
          className="z-50 max-w-96 rounded-md bg-text px-3 py-2 text-[14px] leading-snug break-words whitespace-pre-line text-bg shadow-lg"
        >
          {content}
        </TooltipPrimitive.Content>
      </TooltipPrimitive.Portal>
    </TooltipPrimitive.Root>
  );
}

export interface Option<T extends string> {
  value: T;
  label: string;
  hint?: string;
}

export function Select<T extends string>({
  value,
  options,
  onChange,
  disabled,
  className,
  ariaLabel,
}: {
  value: T;
  options: Option<T>[];
  onChange: (v: T) => void;
  disabled?: boolean;
  className?: string;
  ariaLabel?: string;
}) {
  return (
    <SelectPrimitive.Root value={value} onValueChange={(v) => onChange(v as T)} disabled={disabled}>
      <SelectPrimitive.Trigger
        aria-label={ariaLabel}
        className={clsx(
          "inline-flex h-11 w-full min-w-0 items-center justify-between gap-2 rounded-lg border border-line-strong bg-surface px-3 text-left text-[16px]",
          "hover:bg-sunken disabled:opacity-45",
          className,
        )}
      >
        <span className="truncate">
          <SelectPrimitive.Value />
        </span>
        <SelectPrimitive.Icon>
          <ChevronDown className="size-5 text-muted" />
        </SelectPrimitive.Icon>
      </SelectPrimitive.Trigger>
      <SelectPrimitive.Portal>
        <SelectPrimitive.Content
          position="popper"
          sideOffset={4}
          className="z-50 max-h-[70vh] min-w-[var(--radix-select-trigger-width)] overflow-hidden rounded-lg border border-line bg-surface shadow-xl"
        >
          <SelectPrimitive.Viewport className="p-1.5">
            {options.map((o) => (
              <SelectPrimitive.Item
                key={o.value}
                value={o.value}
                className="relative flex min-h-11 cursor-default flex-col justify-center rounded-md px-3 py-2 pr-10 text-[16px] outline-none data-[highlighted]:bg-accent-soft"
              >
                <SelectPrimitive.ItemText>{o.label}</SelectPrimitive.ItemText>
                {o.hint && <span className="text-[14px] text-muted">{o.hint}</span>}
                <SelectPrimitive.ItemIndicator className="absolute top-1/2 right-3 -translate-y-1/2">
                  <Check className="size-5 text-accent" />
                </SelectPrimitive.ItemIndicator>
              </SelectPrimitive.Item>
            ))}
          </SelectPrimitive.Viewport>
        </SelectPrimitive.Content>
      </SelectPrimitive.Portal>
    </SelectPrimitive.Root>
  );
}

export function Switch({
  checked,
  onChange,
  label,
  hint,
  disabled,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  hint?: string;
  disabled?: boolean;
}) {
  return (
    <label className={clsx("flex min-h-11 items-start gap-3 py-1", disabled ? "opacity-45" : "cursor-pointer")}>
      <SwitchPrimitive.Root
        checked={checked}
        onCheckedChange={onChange}
        disabled={disabled}
        className="relative mt-0.5 h-7 w-12 shrink-0 rounded-full border border-line-strong bg-sunken transition-colors data-[state=checked]:border-accent data-[state=checked]:bg-accent"
      >
        <SwitchPrimitive.Thumb className="block size-[22px] translate-x-[2px] rounded-full bg-white shadow transition-transform data-[state=checked]:translate-x-[22px]" />
      </SwitchPrimitive.Root>
      <span className="min-w-0">
        <span className="block text-[16px]">{label}</span>
        {hint && <span className="block text-[14px] leading-snug text-muted">{hint}</span>}
      </span>
    </label>
  );
}

/** Флажок «убрать этот файл»; крупная область нажатия вокруг квадрата. */
/** `remove` — красный крестик («убрать эту копию»), `select` — синяя галочка («выбрано»). */
export function Checkbox({
  checked,
  onChange,
  label,
  tone = "remove",
  className,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  tone?: "remove" | "select";
  className?: string;
}) {
  return (
    <CheckboxPrimitive.Root
      checked={checked}
      onCheckedChange={(v) => onChange(v === true)}
      aria-label={label}
      onClick={(e) => e.stopPropagation()}
      className={clsx("group flex size-11 shrink-0 items-center justify-center rounded-lg hover:bg-sunken", className)}
    >
      <span
        className={clsx(
          "flex size-6 items-center justify-center rounded-md border-2 border-line-strong bg-surface",
          tone === "remove"
            ? "group-data-[state=checked]:border-err group-data-[state=checked]:bg-err"
            : "group-data-[state=checked]:border-accent group-data-[state=checked]:bg-accent",
        )}
      >
        <CheckboxPrimitive.Indicator>
          {tone === "remove" ? (
            <X className="size-4 text-white" strokeWidth={3} />
          ) : (
            <Check className="size-4 text-white" strokeWidth={3} />
          )}
        </CheckboxPrimitive.Indicator>
      </span>
    </CheckboxPrimitive.Root>
  );
}

export function Slider({
  value,
  min,
  max,
  step = 1,
  onChange,
  label,
}: {
  value: number;
  min: number;
  max: number;
  step?: number;
  onChange: (v: number) => void;
  label: string;
}) {
  return (
    <SliderPrimitive.Root
      value={[value]}
      min={min}
      max={max}
      step={step}
      onValueChange={([v]) => onChange(v)}
      className="relative flex h-11 w-full touch-none items-center select-none"
    >
      <SliderPrimitive.Track className="relative h-1.5 grow rounded-full bg-line">
        <SliderPrimitive.Range className="absolute h-full rounded-full bg-accent" />
      </SliderPrimitive.Track>
      <SliderPrimitive.Thumb
        aria-label={label}
        className="block size-7 rounded-full border-[3px] border-accent bg-surface shadow"
      />
    </SliderPrimitive.Root>
  );
}

export function Field({ label, children, hint }: { label: string; children: ReactNode; hint?: ReactNode }) {
  return (
    <div className="space-y-1.5">
      <div className="text-[16px] font-medium">{label}</div>
      {children}
      {hint && <div className="text-[14px] leading-snug text-muted">{hint}</div>}
    </div>
  );
}

export function Dialog({
  open,
  onOpenChange,
  title,
  children,
  footer,
  width = 520,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  title: string;
  children: ReactNode;
  footer?: ReactNode;
  width?: number;
}) {
  return (
    <DialogPrimitive.Root open={open} onOpenChange={onOpenChange}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-40 bg-[#0b1220]/50" />
        <DialogPrimitive.Content
          aria-describedby={undefined}
          style={{ width }}
          className="fixed top-1/2 left-1/2 z-50 flex max-h-[88vh] max-w-[94vw] -translate-x-1/2 -translate-y-1/2 flex-col rounded-xl border border-line bg-surface shadow-2xl"
        >
          <div className="flex items-center justify-between gap-3 border-b border-line py-2 pr-2 pl-6">
            <DialogPrimitive.Title className="text-[20px] font-semibold">{title}</DialogPrimitive.Title>
            <DialogPrimitive.Close asChild>
              <IconButton label="Закрыть">
                <X className="size-5" />
              </IconButton>
            </DialogPrimitive.Close>
          </div>
          <div className="min-h-0 overflow-y-auto px-6 py-5 text-[16px]">{children}</div>
          {footer && <div className="flex flex-wrap justify-end gap-3 border-t border-line px-6 py-4">{footer}</div>}
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

export function ProgressBar({ value, paused }: { value: number; paused?: boolean }) {
  return (
    <div className="h-3 w-full overflow-hidden rounded-full bg-line">
      <div
        className={clsx("h-full rounded-full transition-[width] duration-300", paused ? "bg-warn" : "bg-accent")}
        style={{ width: `${Math.round(Math.min(1, Math.max(0, value)) * 100)}%` }}
      />
    </div>
  );
}

/** Заголовок шага: основной сценарий виден как последовательность (ТЗ 8.5). */
export function Step({ n, title, children }: { n: number; title: string; children?: ReactNode }) {
  return (
    <div className="flex items-center gap-3">
      <span className="flex size-9 shrink-0 items-center justify-center rounded-full bg-accent text-[18px] font-semibold text-accent-ink">
        {n}
      </span>
      <h2 className="text-[20px] font-semibold">{title}</h2>
      {children}
    </div>
  );
}
