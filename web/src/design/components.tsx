// React wrappers for the design system classes in web/public/design/components.css (H15-11).
// They only add markup, ARIA and state; all styling is in CSS, driven by the tokens.
import { useEffect, useRef, useState } from "react";
import type {
  ButtonHTMLAttributes,
  CSSProperties,
  HTMLAttributes,
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent as ReactMouseEvent,
  PointerEvent as ReactPointerEvent,
  ReactNode,
} from "react";

/** Joins class names, skipping falsy ones. */
export function cx(...names: Array<string | false | null | undefined>): string {
  return names.filter(Boolean).join(" ");
}

export type Tone = "accent" | "success" | "warning" | "critical" | "info" | "muted";

// ---------- Panel ----------

export interface PanelProps extends Omit<HTMLAttributes<HTMLElement>, "title"> {
  title?: ReactNode;
  /** Right side of the header, e.g. a badge or a counter. */
  aside?: ReactNode;
  /** Brighter frame with a glow, for the focused or primary panel. */
  accent?: boolean;
}

export function Panel({ title, aside, accent, className, children, ...rest }: PanelProps) {
  return (
    <section className={cx("sd-frame sd-panel", accent && "sd-panel--accent", className)} {...rest}>
      {(title ?? aside) !== undefined && (
        <header className="sd-panel__header">
          {title}
          {aside !== undefined && <span className="sd-panel__header-aside">{aside}</span>}
        </header>
      )}
      {children}
    </section>
  );
}

export function SectionTitle({ children }: { children: ReactNode }) {
  return <h3 className="sd-section-title">{children}</h3>;
}

/**
 * A section that folds (H15-53): a section title you can click (or Enter) to show or hide its
 * content. Native `<details>`, so it works without script and screen readers know its state.
 */
export function Disclosure({ title, aside, open, children }: { title: ReactNode; aside?: ReactNode; open?: boolean; children: ReactNode }) {
  return (
    <details className="sd-disclosure" open={open}>
      <summary className="sd-disclosure__summary">
        <span className="sd-disclosure__title">{title}</span>
        {aside !== undefined && <span className="sd-disclosure__aside">{aside}</span>}
      </summary>
      <div className="sd-disclosure__body">{children}</div>
    </details>
  );
}

// ---------- Keycap, Button, Badge ----------

export function Keycap({ children }: { children: ReactNode }) {
  return <kbd className="sd-keycap">{children}</kbd>;
}

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: "default" | "primary" | "ghost";
  size?: "md" | "sm";
  /** Keyboard hint shown before the label, e.g. "F1". */
  hint?: ReactNode;
}

export function Button({ variant = "default", size = "md", hint, className, children, type = "button", ...rest }: ButtonProps) {
  return (
    <button
      type={type}
      className={cx("sd-frame sd-button", variant !== "default" && `sd-button--${variant}`, size === "sm" && "sd-button--sm", className)}
      {...rest}
    >
      {hint !== undefined && <Keycap>{hint}</Keycap>}
      {children}
    </button>
  );
}

export function Badge({ tone = "accent", children }: { tone?: Tone; children: ReactNode }) {
  return (
    <span className="sd-frame sd-badge" data-tone={tone}>
      {children}
    </span>
  );
}

// ---------- Tabs ----------

export interface TabsProps<T extends string> {
  tabs: ReadonlyArray<{ id: T; label: ReactNode }>;
  value: NoInfer<T>;
  onChange(id: NoInfer<T>): void;
  /** Keycaps at both ends, like the game's [1] … [3] hints. */
  prevHint?: ReactNode;
  nextHint?: ReactNode;
  label: string;
}

export function Tabs<T extends string>({ tabs, value, onChange, prevHint, nextHint, label }: TabsProps<T>) {
  return (
    <div className="sd-tabs" role="tablist" aria-label={label}>
      {prevHint !== undefined && <Keycap>{prevHint}</Keycap>}
      {tabs.map((tab) => (
        <button
          key={tab.id}
          type="button"
          role="tab"
          className="sd-tabs__tab"
          aria-selected={tab.id === value}
          onClick={() => onChange(tab.id)}
        >
          {tab.label}
        </button>
      ))}
      {nextHint !== undefined && <Keycap>{nextHint}</Keycap>}
    </div>
  );
}

// ---------- Menu list ----------

export function MenuList({ children, label }: { children: ReactNode; label?: string }) {
  return (
    <ul className="sd-list" aria-label={label}>
      {children}
    </ul>
  );
}

export interface MenuRowProps {
  label: ReactNode;
  selected?: boolean;
  children?: ReactNode;
  onSelect?(): void;
}

export function MenuRow({ label, selected, children, onSelect }: MenuRowProps) {
  return (
    <li className="sd-list__row" data-selected={selected ? "true" : undefined} onPointerEnter={onSelect}>
      <span className="sd-list__label">{label}</span>
      {children !== undefined && <span className="sd-list__control">{children}</span>}
    </li>
  );
}

// ---------- Toggle, Stepper, Slider ----------

export interface ToggleProps {
  checked: boolean;
  onChange(checked: boolean): void;
  label: string;
  offLabel?: string;
  onLabel?: string;
  disabled?: boolean;
}

export function Toggle({ checked, onChange, label, offLabel = "Off", onLabel = "On", disabled }: ToggleProps) {
  return (
    <button
      type="button"
      role="switch"
      className="sd-toggle"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
    >
      <span className="sd-toggle__side sd-toggle__off">{offLabel}</span>
      <span className="sd-toggle__side sd-toggle__on">{onLabel}</span>
    </button>
  );
}

export interface StepperProps<T extends string> {
  options: readonly T[];
  value: NoInfer<T>;
  onChange(value: NoInfer<T>): void;
  label: string;
  /** Wrap around at the ends (default) or stop. */
  wrap?: boolean;
}

export function Stepper<T extends string>({ options, value, onChange, label, wrap = true }: StepperProps<T>) {
  const i = Math.max(0, options.indexOf(value));
  const n = options.length;
  const step = (d: number) => {
    const j = wrap ? (i + d + n) % n : Math.min(n - 1, Math.max(0, i + d));
    const next = options[j];
    if (next !== undefined) onChange(next);
  };
  return (
    <div className="sd-stepper" role="group" aria-label={label}>
      <button type="button" className="sd-stepper__arrow" aria-label={`Previous ${label}`} disabled={!wrap && i === 0} onClick={() => step(-1)}>
        ◁
      </button>
      <span className="sd-stepper__value" aria-live="polite">
        {value}
      </span>
      <button type="button" className="sd-stepper__arrow" aria-label={`Next ${label}`} disabled={!wrap && i === n - 1} onClick={() => step(1)}>
        ▷
      </button>
    </div>
  );
}

export interface SliderProps {
  value: number;
  onChange(value: number): void;
  /** Called once when the user lets go (pointer up, or a key released), e.g. to send a command. */
  onCommit?(value: number): void;
  label: string;
  min?: number;
  max?: number;
  step?: number;
}

export function Slider({ value, onChange, onCommit, label, min = 0, max = 100, step = 1 }: SliderProps) {
  const pct = ((value - min) / (max - min || 1)) * 100;
  return (
    <input
      type="range"
      className="sd-slider"
      aria-label={label}
      min={min}
      max={max}
      step={step}
      value={value}
      style={{ "--sd-value": `${pct}%` } as CSSProperties}
      onChange={(e) => onChange(Number(e.currentTarget.value))}
      onPointerUp={onCommit && ((e) => onCommit(Number(e.currentTarget.value)))}
      onKeyUp={onCommit && ((e) => onCommit(Number(e.currentTarget.value)))}
    />
  );
}

// ---------- XYPad ----------

export interface XYPadProps {
  /** -1 ..= 1, left to right. */
  x: number;
  /** -1 ..= 1, bottom to top. */
  y: number;
  /** While dragging. */
  onChange(x: number, y: number): void;
  /** Once when the user lets go (pointer up, key up). */
  onCommit?(x: number, y: number): void;
  label: string;
  /** Axis labels at the four edges. */
  left: ReactNode;
  right: ReactNode;
  top: ReactNode;
  bottom: ReactNode;
}

const clampUnit = (v: number) => Math.max(-1, Math.min(1, v));

/** The pad position of a pointer event, as -1..1 on both axes. */
function padPoint(e: ReactPointerEvent<HTMLDivElement>): [number, number] {
  const r = e.currentTarget.getBoundingClientRect();
  return [clampUnit(((e.clientX - r.left) / r.width) * 2 - 1), clampUnit(1 - ((e.clientY - r.top) / r.height) * 2)];
}

const PAD_KEYS: Record<string, [number, number]> = {
  ArrowLeft: [-0.1, 0],
  ArrowRight: [0.1, 0],
  ArrowUp: [0, 0.1],
  ArrowDown: [0, -0.1],
};

/** A 2D control (e.g. a sound character: warm↔bright, relaxed↔energetic); arrows move it by 0.1. */
export function XYPad({ x, y, onChange, onCommit, label, left, right, top, bottom }: XYPadProps) {
  const dragging = useRef(false);
  const style = { "--sd-x": `${((x + 1) / 2) * 100}%`, "--sd-y": `${((1 - y) / 2) * 100}%` } as CSSProperties;
  return (
    <div className="sd-xypad">
      <span className="sd-xypad__label sd-xypad__label--top">{top}</span>
      <span className="sd-xypad__label sd-xypad__label--bottom">{bottom}</span>
      <span className="sd-xypad__label sd-xypad__label--left">{left}</span>
      <span className="sd-xypad__label sd-xypad__label--right">{right}</span>
      <div
        className="sd-xypad__area"
        role="slider"
        tabIndex={0}
        aria-label={label}
        aria-valuetext={`${x.toFixed(1)}, ${y.toFixed(1)}`}
        style={style}
        onPointerDown={(e) => {
          dragging.current = true;
          e.currentTarget.setPointerCapture(e.pointerId);
          onChange(...padPoint(e));
        }}
        onPointerMove={(e) => dragging.current && onChange(...padPoint(e))}
        onPointerUp={(e) => {
          dragging.current = false;
          onCommit?.(...padPoint(e));
        }}
        onKeyDown={(e) => {
          const step = PAD_KEYS[e.key];
          if (!step) return;
          e.preventDefault();
          const next: [number, number] = [clampUnit(x + step[0]), clampUnit(y + step[1])];
          onChange(...next);
          onCommit?.(...next);
        }}
      >
        <span className="sd-xypad__dot" />
      </div>
    </div>
  );
}

// ---------- Readout, Meter, DataList ----------

export interface ReadoutProps {
  value: ReactNode;
  label?: ReactNode;
  unit?: ReactNode;
  size?: "sm" | "md" | "lg";
}

export function Readout({ value, label, unit, size = "md" }: ReadoutProps) {
  return (
    <div className={cx("sd-readout", size !== "md" && `sd-readout--${size}`)}>
      <span className="sd-readout__value">
        {value}
        {unit !== undefined && <span className="sd-readout__unit">{unit}</span>}
      </span>
      {label !== undefined && <span className="sd-label">{label}</span>}
    </div>
  );
}

export interface MeterProps {
  /** 0..1 */
  value: number;
  label: string;
  tone?: Exclude<Tone, "muted" | "info">;
}

export function Meter({ value, label, tone = "accent" }: MeterProps) {
  const v = Math.min(1, Math.max(0, value));
  return (
    <div
      className="sd-meter"
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(v * 100)}
      data-tone={tone === "accent" ? undefined : tone}
      style={{ "--sd-value": v } as CSSProperties}
    >
      <span className="sd-meter__fill" />
    </div>
  );
}

export interface DataListProps {
  items: ReadonlyArray<{ term: ReactNode; value: ReactNode }>;
  /** Label and value side by side from 480 px up (default: stacked, like the briefing card). */
  inline?: boolean;
}

export function DataList({ items, inline }: DataListProps) {
  return (
    <dl className={cx("sd-datalist", inline && "sd-datalist--inline")}>
      {items.map((item, i) => (
        <div className="sd-datalist__item" key={i}>
          <dt>{item.term}</dt>
          <dd>{item.value}</dd>
        </div>
      ))}
    </dl>
  );
}

export function HintBar({ hints }: { hints: ReadonlyArray<{ key: ReactNode; label: ReactNode }> }) {
  return (
    <div className="sd-hints">
      {hints.map((h, i) => (
        <span className="sd-hints__item" key={i}>
          <Keycap>{h.key}</Keycap>
          {h.label}
        </span>
      ))}
    </div>
  );
}

// ---------- Dock controls ----------

export interface KeyTileProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "onClick" | "type"> {
  /** Key number as in device.proto: 1..6 LCD keys, 7..9 plain buttons. */
  number: number;
  pressed?: boolean;
  /** Image currently shown on an LCD key (data: or object URL), upright. */
  image?: string;
  label?: ReactNode;
  onClick?(): void;
  /** Change it (e.g. an event counter) to replay the release flash; 0 or undefined shows none. */
  pulse?: number;
}

export function KeyTile({ number, pressed, image, label, onClick, pulse, className, ...rest }: KeyTileProps) {
  const lcd = number <= 6;
  return (
    <button
      {...rest}
      type="button"
      className={cx("sd-frame sd-keytile", !lcd && "sd-keytile--button", className)}
      data-state={pressed ? "pressed" : undefined}
      aria-pressed={pressed}
      aria-label={`${lcd ? "Key" : "Button"} ${number}`}
      onClick={onClick}
    >
      {pulse !== undefined && pulse > 0 && <span key={pulse} className="sd-keytile__flash" aria-hidden />}
      <span className="sd-keytile__number">{number}</span>
      {image !== undefined ? <img className="sd-keytile__image" src={image} alt="" /> : (label ?? (lcd ? "—" : `B${number}`))}
    </button>
  );
}

export interface KnobProps extends Omit<HTMLAttributes<HTMLDivElement>, "role"> {
  /** Knob number as in device.proto: 1..3. */
  number: number;
  /** Accumulated detents; the indicator turns 15° per detent. */
  position: number;
  pressed?: boolean;
  label?: ReactNode;
  /** Direction of the last detent: plays the clockwise or counter-clockwise turn flash. */
  turn?: -1 | 0 | 1;
  /** Change it per event (e.g. an event counter) to replay the turn flash for repeated detents. */
  pulse?: number;
  /**
   * Makes the knob a control (H15-51): drag the dial around, the wheel, arrow keys or the ‹ ›
   * buttons turn it (detents, positive clockwise).
   */
  onTurn?(detents: number): void;
  /** With `onTurn`: a click or tap on the dial, Enter or Space presses it. */
  onPress?(): void;
}

/** Degrees of a drag around the dial per detent, as the indicator shows it. */
const DEGREES_PER_DETENT = 15;
/** Wheel distance (pixels) per detent: one mouse wheel notch. */
const WHEEL_PER_DETENT = 100;

function pointerAngle(el: HTMLElement, e: ReactPointerEvent): number {
  const r = el.getBoundingClientRect();
  return (Math.atan2(e.clientY - (r.top + r.height / 2), e.clientX - (r.left + r.width / 2)) * 180) / Math.PI;
}

/** Turns `delta` degrees into whole detents, keeping the remainder in `rest`. */
function takeDetents(rest: { current: number }, delta: number, per: number): number {
  rest.current += delta;
  const detents = Math.trunc(rest.current / per);
  rest.current -= detents * per;
  return detents;
}

/** Wheel input on `ref` (a native listener: React's is passive and cannot stop the page scrolling). */
function useWheelTurn(ref: { current: HTMLElement | null }, onTurn: ((detents: number) => void) | undefined) {
  const turn = useRef(onTurn);
  turn.current = onTurn;
  const enabled = onTurn !== undefined;
  useEffect(() => {
    const el = ref.current;
    if (!el || !enabled) return;
    const rest = { current: 0 };
    const wheel = (e: WheelEvent) => {
      e.preventDefault();
      const px = e.deltaMode === WheelEvent.DOM_DELTA_PIXEL ? e.deltaY : e.deltaY * 40;
      // Wheel up turns clockwise (louder, brighter).
      const detents = takeDetents(rest, -px, WHEEL_PER_DETENT);
      if (detents !== 0) turn.current?.(detents);
    };
    el.addEventListener("wheel", wheel, { passive: false });
    return () => el.removeEventListener("wheel", wheel);
  }, [ref, enabled]);
}

/** Props for an interactive dial: drag around it to turn, click to press, keys. */
function useDialControl(onTurn: ((detents: number) => void) | undefined, onPress: (() => void) | undefined) {
  const ref = useRef<HTMLSpanElement>(null);
  const drag = useRef<{ angle: number; turned: boolean } | null>(null);
  const rest = useRef(0);
  useWheelTurn(ref, onTurn);
  if (!onTurn) return { ref };
  return {
    ref,
    tabIndex: 0,
    onPointerDown(e: ReactPointerEvent<HTMLSpanElement>) {
      if (e.button !== 0) return;
      e.currentTarget.setPointerCapture(e.pointerId);
      drag.current = { angle: pointerAngle(e.currentTarget, e), turned: false };
      rest.current = 0;
    },
    onPointerMove(e: ReactPointerEvent<HTMLSpanElement>) {
      const d = drag.current;
      if (!d) return;
      const angle = pointerAngle(e.currentTarget, e);
      // Shortest way round, so crossing ±180° is no jump.
      const delta = ((angle - d.angle + 540) % 360) - 180;
      d.angle = angle;
      const detents = takeDetents(rest, delta, DEGREES_PER_DETENT);
      if (detents !== 0) {
        d.turned = true;
        onTurn(detents);
      }
    },
    // Pointer capture ends by itself on release; the click that follows a drag is no press.
    onClick() {
      const turned = drag.current?.turned ?? false;
      drag.current = null;
      if (!turned) onPress?.();
    },
    onKeyDown(e: ReactKeyboardEvent) {
      const step = { ArrowRight: 1, ArrowUp: 1, ArrowLeft: -1, ArrowDown: -1 }[e.key];
      if (step !== undefined) onTurn(step);
      else if (e.key === "Enter" || e.key === " ") onPress?.();
      else return;
      e.preventDefault();
    },
  };
}

/** The ‹ › buttons under a knob you can turn: one detent each, for touch screens. */
function KnobSteps({ name, onTurn }: { name: string; onTurn(detents: number): void }) {
  return (
    <span className="sd-knob__steps">
      <button type="button" className="sd-knob__step" aria-label={`Turn ${name} left`} onClick={() => onTurn(-1)}>
        ‹
      </button>
      <button type="button" className="sd-knob__step" aria-label={`Turn ${name} right`} onClick={() => onTurn(1)}>
        ›
      </button>
    </span>
  );
}

/** What a knob tells screen readers: a picture of its state, or a control. */
function knobAria(name: string, position: number, pressed: boolean | undefined, control: boolean) {
  if (!control) return { outer: { role: "img", "aria-label": `${name}, position ${position}${pressed ? ", pressed" : ""}` }, dial: {} };
  return {
    outer: { role: "group", "aria-label": name },
    dial: {
      role: "spinbutton",
      "aria-label": `${name}: turn with arrow keys or by dragging, press with Enter`,
      "aria-valuenow": position,
      "aria-valuetext": `position ${position}`,
    },
  };
}

export function Knob({ number, position, pressed, label, turn = 0, pulse, onTurn, onPress, className, style, ...rest }: KnobProps) {
  const dial = useDialControl(onTurn, onPress);
  const name = `Knob ${number}`;
  const aria = knobAria(name, position, pressed, onTurn !== undefined);
  return (
    <div
      {...rest}
      {...aria.outer}
      className={cx("sd-knob", onTurn && "sd-knob--control", className)}
      data-state={pressed ? "pressed" : undefined}
      style={{ ...style, "--sd-angle": `${position * 15}deg` } as CSSProperties}
    >
      {/* The grip stays mounted while the dial remounts per detent (to replay its flash): it keeps a drag's pointer capture. */}
      <span {...dial} {...aria.dial} className="sd-knob__grip">
        <span key={pulse} className={cx("sd-knob__dial", turn > 0 && "is-cw", turn < 0 && "is-ccw")}>
          <span className="sd-knob__indicator" />
        </span>
      </span>
      <span className="sd-knob__label">
        {label ?? `K${number}`} <span className="sd-knob__pos">{position > 0 ? "+" : ""}{position}</span>
      </span>
      {onTurn && <KnobSteps name={name} onTurn={onTurn} />}
    </div>
  );
}

// ---------- Effects ----------

/** Glitched text; the visual copies are hidden from screen readers, which read `text` once. */
export function Glitch({ text }: { text: string }) {
  return (
    <>
      <span className="sd-glitch" data-text={text} aria-hidden>
        {text}
      </span>
      <span className="sd-visually-hidden">{text}</span>
    </>
  );
}

// ---------- Context menu (H15-32) ----------

export interface ContextMenuItem {
  label: ReactNode;
  onSelect(): void;
  disabled?: boolean;
  /** Shown dimmed after the label, e.g. the current value. */
  hint?: ReactNode;
}

export interface ContextMenuState {
  x: number;
  y: number;
  title?: ReactNode;
  items: ContextMenuItem[];
}

/**
 * A floating menu at a point (right click, or long press on touch). Closes on Escape, a click
 * outside or a choice; arrow keys move between items.
 */
export function ContextMenu({ menu, onClose }: { menu: ContextMenuState | null; onClose(): void }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!menu) return;
    ref.current?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();
    const away = (e: Event) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      e.preventDefault();
      const buttons = [...(ref.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? [])];
      const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
      const next = (at + (e.key === "ArrowDown" ? 1 : buttons.length - 1)) % buttons.length;
      buttons[next]?.focus();
    };
    document.addEventListener("pointerdown", away, true);
    document.addEventListener("keydown", key);
    window.addEventListener("resize", onClose);
    return () => {
      document.removeEventListener("pointerdown", away, true);
      document.removeEventListener("keydown", key);
      window.removeEventListener("resize", onClose);
    };
  }, [menu, onClose]);
  if (!menu) return null;
  // Keep the menu on screen: open left / up when there is no room.
  const style: CSSProperties = {
    left: Math.max(8, Math.min(menu.x, window.innerWidth - 248)),
    top: Math.max(8, Math.min(menu.y, window.innerHeight - 48 * (menu.items.length + 1) - 16)),
  };
  return (
    <div ref={ref} className="sd-frame sd-context-menu" role="menu" style={style}>
      {menu.title !== undefined && <div className="sd-context-menu__title">{menu.title}</div>}
      {menu.items.map((item, i) => (
        <button
          key={i}
          type="button"
          role="menuitem"
          className="sd-context-menu__item"
          disabled={item.disabled}
          onClick={() => {
            onClose();
            item.onSelect();
          }}
        >
          <span>{item.label}</span>
          {item.hint !== undefined && <span className="sd-context-menu__hint">{item.hint}</span>}
        </button>
      ))}
    </div>
  );
}

/** How long a touch must be held to open the context menu. */
const LONG_PRESS_MS = 500;

/**
 * Props that open a context menu on right click and on a long press (touch), for any element.
 * `open` gets the position; a long press also suppresses the click that follows it.
 */
export function useContextMenuTrigger(open: (x: number, y: number) => void) {
  const timer = useRef<number | undefined>(undefined);
  const fired = useRef(false);
  const cancel = () => window.clearTimeout(timer.current);
  return {
    onContextMenu(e: ReactMouseEvent) {
      e.preventDefault();
      cancel();
      open(e.clientX, e.clientY);
    },
    onPointerDown(e: ReactPointerEvent) {
      fired.current = false;
      if (e.pointerType !== "touch") return;
      const { clientX, clientY } = e;
      timer.current = window.setTimeout(() => {
        fired.current = true;
        open(clientX, clientY);
      }, LONG_PRESS_MS);
    },
    onPointerUp: cancel,
    onPointerLeave: cancel,
    onPointerMove(e: ReactPointerEvent) {
      if (e.pointerType === "touch" && (Math.abs(e.movementX) > 4 || Math.abs(e.movementY) > 4)) cancel();
    },
    onClickCapture(e: ReactMouseEvent) {
      if (fired.current) {
        e.preventDefault();
        e.stopPropagation();
        fired.current = false;
      }
    },
  };
}

// ---------- Dialog (H15-32) ----------

export interface DialogProps {
  open: boolean;
  title: ReactNode;
  onClose(): void;
  children: ReactNode;
  /** Buttons at the bottom, e.g. Cancel / Save. */
  actions?: ReactNode;
}

/** A modal dialog on the native <dialog> element (focus trap, Escape and backdrop for free). */
export function Dialog({ open, title, onClose, children, actions }: DialogProps) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  }, [open]);
  return (
    <dialog
      ref={ref}
      className="sd-frame sd-dialog"
      onClose={onClose}
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
      onClick={(e) => {
        if (e.target === ref.current) onClose();
      }}
    >
      {open && (
        <>
          <header className="sd-panel__header">{title}</header>
          <div className="sd-dialog__body">{children}</div>
          {actions !== undefined && <footer className="sd-dialog__actions">{actions}</footer>}
        </>
      )}
    </dialog>
  );
}

// ---------- Inline rename (H15-32) ----------

/** Text that turns into an input on double click; Enter saves, Escape cancels. */
export function InlineEdit({
  value,
  onSave,
  label,
  editing: editingProp,
  onEditingChange,
}: {
  value: string;
  onSave(value: string): void;
  label: string;
  editing?: boolean;
  onEditingChange?(editing: boolean): void;
}) {
  const [own, setOwn] = useState(false);
  const editing = editingProp ?? own;
  const setEditing = (on: boolean) => (onEditingChange ? onEditingChange(on) : setOwn(on));
  const [text, setText] = useState(value);
  useEffect(() => setText(value), [value, editing]);
  if (!editing) {
    return (
      <span className="sd-inline-edit" onDoubleClick={() => setEditing(true)} title="Double-click to rename">
        {value}
      </span>
    );
  }
  const done = (save: boolean) => {
    setEditing(false);
    const t = text.trim();
    if (save && t && t !== value) onSave(t);
  };
  return (
    <input
      className="sd-input sd-inline-edit__input"
      aria-label={label}
      autoFocus
      value={text}
      size={Math.max(4, text.length + 1)}
      onChange={(e) => setText(e.currentTarget.value)}
      onClick={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        if (e.key === "Enter") done(true);
        if (e.key === "Escape") done(false);
      }}
      onBlur={() => done(true)}
    />
  );
}

/* ---------- Now playing (H15-88) ---------- */

export interface NowPlayingProps {
  /** The line that scrolls, e.g. "Title · Artist". */
  text: string;
  coverUrl?: string;
  playing?: boolean;
  paused?: boolean;
  /** Nothing loaded: shows `idleText`. */
  idle?: boolean;
  /** The player or the server is unreachable. */
  offline?: boolean;
  idleText?: string;
  durationMs?: number;
  /** Position at `elapsedAtMs` (unix ms); counted on while playing. */
  elapsedMs?: number;
  elapsedAtMs?: number;
  /** Scroll long text (else cut it with "…"), at this speed, pausing at each end. */
  scroll?: boolean;
  speed?: number;
  pauseMs?: number;
  showCover?: boolean;
  showProgress?: boolean;
  /** Draw only part of a band `count` tiles wide (the dashboard's dock keys). */
  slice?: { index: number; count: number };
}

/** Scroll position like the dock's (`marquee_offset`): still, moving left, still at the end, again. */
export function marqueeOffset(ms: number, distance: number, speed: number, pauseMs: number): number {
  if (distance <= 0 || speed <= 0) return 0;
  const travel = (distance / speed) * 1000;
  const t = ms % (pauseMs * 2 + travel);
  return (Math.min(Math.max(t - pauseMs, 0), travel) / 1000) * speed;
}

function prefersReducedMotion(): boolean {
  return typeof window !== "undefined" && window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
}

interface Timing {
  moving: boolean;
  speed: number;
  pauseMs: number;
  playing: boolean;
  durationMs: number;
  elapsedMs: number;
  elapsedAtMs: number;
}

/** Scrolls the line inside the band (or puts it back at the start). */
function moveText(band: HTMLElement | null, line: HTMLElement | null, t: Timing, ms: number) {
  if (!band || !line) return;
  const x = t.moving ? marqueeOffset(ms, line.scrollWidth - band.clientWidth, t.speed, t.pauseMs) : 0;
  line.style.transform = x ? `translateX(${-x}px)` : "";
}

/** Sets the progress bar from the last known position, counted on while playing. */
function moveBar(bar: HTMLElement | null, t: Timing) {
  if (!bar || t.durationMs <= 0) return;
  const at = t.elapsedMs + (t.playing && t.elapsedAtMs ? Date.now() - t.elapsedAtMs : 0);
  bar.style.width = `${Math.min(Math.max(at / t.durationMs, 0), 1) * 100}%`;
}

/** One requestAnimationFrame loop for the line and the bar, restarted when `key` changes. */
function useNowPlayingFrames(key: string, t: Timing) {
  const bandRef = useRef<HTMLSpanElement>(null);
  const textRef = useRef<HTMLSpanElement>(null);
  const barRef = useRef<HTMLSpanElement>(null);
  const timing = useRef(t);
  timing.current = t;
  useEffect(() => {
    let frame = 0;
    const started = performance.now();
    const tick = () => {
      moveText(bandRef.current, textRef.current, timing.current, performance.now() - started);
      moveBar(barRef.current, timing.current);
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [key, t.moving]);
  return { bandRef, textRef, barRef };
}

function nowPlayingState({ offline, idle, paused, playing }: NowPlayingProps): string {
  if (offline) return "offline";
  if (idle) return "idle";
  return paused || !playing ? "paused" : "playing";
}

function shownText({ offline, idle, idleText = "Nothing playing", text }: NowPlayingProps): string {
  if (offline) return "Offline";
  return idle ? idleText : text;
}

function timingOf(p: NowPlayingProps, active: boolean): Timing {
  return {
    moving: (p.scroll ?? true) && Boolean(p.playing) && active && !prefersReducedMotion(),
    speed: p.speed ?? 40,
    pauseMs: p.pauseMs ?? 1500,
    playing: Boolean(p.playing),
    durationMs: p.durationMs ?? 0,
    elapsedMs: p.elapsedMs ?? 0,
    elapsedAtMs: p.elapsedAtMs ?? 0,
  };
}

/**
 * The current track of a player: one line that scrolls when it doesn't fit, an optional cover
 * behind it and a progress bar. Animated with requestAnimationFrame (no React re-render per frame);
 * with reduced motion the line is cut with "…".
 */
function sliceStyle(slice: NowPlayingProps["slice"]): CSSProperties | undefined {
  return slice ? ({ "--sd-np-count": slice.count, "--sd-np-index": slice.index } as CSSProperties) : undefined;
}

/** What the component shows besides the text: the cover URL and whether the bar is drawn. */
function nowPlayingParts(props: NowPlayingProps, active: boolean, t: Timing) {
  return {
    cover: (props.showCover ?? true) && active ? props.coverUrl : undefined,
    bar: (props.showProgress ?? true) && active && t.durationMs > 0,
  };
}

export function NowPlaying(props: NowPlayingProps) {
  const active = !props.idle && !props.offline;
  const shown = shownText(props);
  const t = timingOf(props, active);
  const { bandRef, textRef, barRef } = useNowPlayingFrames(`${shown}|${t.speed}|${t.pauseMs}`, t);
  const { cover, bar } = nowPlayingParts(props, active, t);
  return (
    <span
      className={cx("sd-nowplaying", props.slice && "sd-nowplaying--slice")}
      data-state={nowPlayingState(props)}
      style={sliceStyle(props.slice)}
    >
      <span className="sd-nowplaying__band" ref={bandRef}>
        {cover && <img className="sd-nowplaying__cover" src={cover} alt="" loading="lazy" />}
        <span className={cx("sd-nowplaying__text", !t.moving && "is-cut")} ref={textRef}>
          {shown}
        </span>
        {bar && (
          <span className="sd-nowplaying__progress">
            <span className="sd-nowplaying__bar" ref={barRef} />
          </span>
        )}
      </span>
    </span>
  );
}
