// A select over labelled choices from GetSettingChoices.options (H15-87): Lyrion players are stored by
// MAC and shown by name, with their state. A value that isn't among the choices (an older config with
// the player's name, a player that is gone) is matched by label, else kept as an extra option.
import type { SettingChoice } from "../gen/streamdock/v1/device_pb.js";

function text(c: Pick<SettingChoice, "label" | "detail">): string {
  return c.detail ? `${c.label} (${c.detail})` : c.label;
}

/** The choice `value` stands for: by value, else by label (case-insensitive). */
export function matchChoice(options: readonly SettingChoice[], value: string): SettingChoice | undefined {
  const v = value.trim().toLowerCase();
  return options.find((o) => o.value.toLowerCase() === v) ?? options.find((o) => o.label.toLowerCase() === v);
}

export interface ChoiceSelectProps {
  id?: string;
  label?: string;
  options: readonly SettingChoice[];
  value: string;
  onChange(value: string): void;
  /** An extra first option with value "" (e.g. "The default player"). */
  emptyLabel?: string;
}

export function ChoiceSelect({ id, label, options, value, onChange, emptyLabel }: ChoiceSelectProps) {
  const current = matchChoice(options, value);
  const unknown = value.trim() !== "" && !current;
  return (
    <select id={id} aria-label={label} className="sd-input" value={current?.value ?? value} onChange={(e) => onChange(e.currentTarget.value)}>
      {emptyLabel !== undefined && <option value="">{emptyLabel}</option>}
      {unknown && <option value={value}>{value} (not found)</option>}
      {options.map((o) => (
        <option key={o.value} value={o.value}>
          {text(o)}
        </option>
      ))}
    </select>
  );
}
