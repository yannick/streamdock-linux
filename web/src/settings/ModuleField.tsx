// One field of a module's settings form (H15-22). Fields with suggestions load them from the daemon:
// plain values (Home Assistant areas) as a datalist on the text input, labelled ones (Lyrion players,
// stored by MAC and shown by name, H15-87) as a select, loaded as soon as the form has the URL.
import { useEffect, useState } from "react";

import { deviceClient } from "../api/client.js";
import { Button } from "../design/index.js";
import { SettingType, type SettingChoice, type SettingField } from "../gen/streamdock/v1/device_pb.js";
import { ChoiceSelect } from "./ChoiceSelect.js";

function inputType(type: SettingType): string {
  switch (type) {
    case SettingType.URL:
      return "url";
    case SettingType.INTEGER:
      return "number";
    default:
      return "text";
  }
}

function message(err: unknown): string {
  return err instanceof Error ? err.message.replace(/^\[\w+\]\s*/, "") : String(err);
}

/** Labelled options: shown as a select instead of a datalist. */
function labelled(options: readonly SettingChoice[] | undefined): options is SettingChoice[] {
  return options !== undefined && options.some((o) => o.label !== o.value);
}

interface ModuleFieldProps {
  moduleId: string;
  field: SettingField;
  values: Record<string, string>;
  onChange(value: string): void;
}

export function ModuleField({ moduleId, field, values, onChange }: ModuleFieldProps) {
  const [options, setOptions] = useState<SettingChoice[] | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const id = `${moduleId}-${field.key}`;
  const load = () => {
    setError(null);
    deviceClient
      .getSettingChoices({ module: moduleId, key: field.key, values })
      .then((res) => {
        setOptions(res.options);
        // An empty field takes the default (none = the first player listed), stored by its value (MAC).
        const fallback = field.defaultValue
          ? res.options.find((o) => o.label === field.defaultValue)
          : res.options[0];
        if (!(values[field.key] ?? "").trim() && fallback) onChange(fallback.value);
      })
      .catch((err: unknown) => setError(message(err)));
  };
  // Players are picked from the list, so it loads by itself once there is a URL to ask.
  const url = values.url ?? "";
  useEffect(() => {
    if (field.hasChoices && moduleId === "lyrion" && url.trim()) load();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- reload only when the URL changes
  }, [field.hasChoices, moduleId, url]);
  const value = values[field.key] ?? "";
  const select = labelled(options);
  return (
    <div className="sd-setting">
      <label className="sd-setting__label" htmlFor={id}>
        {field.label}
        {field.required && <span className="sd-setting__required"> *</span>}
      </label>
      <div className="sd-setting__row">
        {select ? (
          <ChoiceSelect id={id} options={options} value={value} onChange={onChange} emptyLabel={emptyLabel(field)} />
        ) : (
          <PlainInput id={id} field={field} value={value} onChange={onChange} suggestions={options !== undefined} />
        )}
        {field.hasChoices && (
          <Button size="sm" variant="ghost" onClick={load}>
            {select ? "Refresh" : "Suggest"}
          </Button>
        )}
      </div>
      {options && !select && <Suggestions id={id} options={options} />}
      {error && <span className="sd-setting__error">{error}</span>}
      <span className="sd-setting__help">{field.help}</span>
    </div>
  );
}

/** For an optional field: an empty choice that stands for the default. */
function emptyLabel(field: SettingField): string | undefined {
  if (field.required) return undefined;
  return field.defaultValue ? `Default: ${field.defaultValue}` : "None";
}

function PlainInput(props: { id: string; field: SettingField; value: string; suggestions: boolean; onChange(v: string): void }) {
  return (
    <input
      id={props.id}
      className="sd-input"
      type={inputType(props.field.type)}
      placeholder={props.field.defaultValue || undefined}
      value={props.value}
      list={props.suggestions ? `${props.id}-choices` : undefined}
      onChange={(e) => props.onChange(e.currentTarget.value)}
    />
  );
}

function Suggestions({ id, options }: { id: string; options: readonly SettingChoice[] }) {
  return (
    <>
      <datalist id={`${id}-choices`}>
        {options.map((o) => (
          <option key={o.value} value={o.value} />
        ))}
      </datalist>
      <span className="sd-setting__help">{options.length} suggestions: start typing or pick from the list.</span>
    </>
  );
}
