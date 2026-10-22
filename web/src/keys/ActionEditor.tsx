// Edits one custom key action (H15-29), recursively for multi steps and toggle sides. Every change is
// a fresh copy passed to `onChange`; the daemon validates on save. One small component per action
// type keeps each part simple (H15-36 complexity limits).
import { clone, create } from "@bufbuild/protobuf";
import { useId, type ReactNode } from "react";

import { Button } from "../design/index.js";
import {
  KeyActionSchema,
  MultiStepSchema,
  type AssistAction,
  type AudioAction,
  type BrightnessAction,
  type CommandAction,
  type HomeAssistantAction,
  type HttpAction,
  type KeyAction,
  type LyrionAction,
  type ModeAction,
  type MultiAction,
  type ProfileAction,
  type ToggleAction,
} from "../gen/streamdock/v1/device_pb.js";
import { ImagePicker } from "./ImagePicker.js";
import { ACTION_TYPES, newAction, type ActionType, type Choices } from "./model.js";

/** Nested editors stop here, so a page can't grow without bounds. */
const MAX_DEPTH = 3;

export function Field({ label, help, children }: { label: string; help?: ReactNode; children(id: string): ReactNode }) {
  const id = useId();
  return (
    <div className="sd-setting">
      <label className="sd-setting__label" htmlFor={id}>
        {label}
      </label>
      {children(id)}
      {help && <span className="sd-setting__help">{help}</span>}
    </div>
  );
}

interface TextProps {
  label: string;
  value: string;
  onChange(value: string): void;
  help?: ReactNode;
  placeholder?: string;
  /** Rows for a multi-line field; a single line without. */
  rows?: number;
  type?: "text" | "url";
}

/** A labelled text input, or a textarea with `rows`. */
function Text({ label, value, onChange, help, placeholder, rows, type = "text" }: TextProps) {
  return (
    <Field label={label} help={help}>
      {(id) =>
        rows ? (
          <textarea
            id={id}
            className="sd-input sd-action__text"
            rows={rows}
            placeholder={placeholder}
            value={value}
            onChange={(e) => onChange(e.currentTarget.value)}
          />
        ) : (
          <input
            id={id}
            className="sd-input"
            type={type}
            placeholder={placeholder}
            value={value}
            onChange={(e) => onChange(e.currentTarget.value)}
          />
        )
      }
    </Field>
  );
}

/** A labelled select of plain strings, with "Choose…" for none. */
function Choice({ label, value, options, onChange, help }: { label: string; value: string; options: string[]; onChange(v: string): void; help?: ReactNode }) {
  return (
    <Field label={label} help={help}>
      {(id) => (
        <select id={id} className="sd-input" value={value} onChange={(e) => onChange(e.currentTarget.value)}>
          <option value="">Choose…</option>
          {options.map((o) => (
            <option key={o}>{o}</option>
          ))}
        </select>
      )}
    </Field>
  );
}

function lines(text: string): string[] {
  return text
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l !== "");
}

/** What every per-type editor gets: its value, a way to change it, and the editor's context. */
interface PartProps<T> {
  value: T;
  set(fn: (value: T) => void): void;
  choices: Choices;
  /** Help text for knob turns about `{delta}`; undefined for presses. */
  delta?: ReactNode;
  depth: number;
}

interface ActionEditorProps {
  action: KeyAction;
  onChange(next: KeyAction): void;
  choices: Choices;
  /** For a knob turn: mention `{delta}`. */
  turn?: boolean;
  depth?: number;
}

export function ActionEditor({ action, onChange, choices, turn = false, depth = 0 }: ActionEditorProps) {
  const edit = (fn: (draft: KeyAction) => void) => {
    const next = clone(KeyActionSchema, action);
    fn(next);
    onChange(next);
  };
  const types = ACTION_TYPES.filter(
    (t) =>
      // Push-to-talk listens while a key is held: only directly on a key (H15-43).
      (t.type !== "assist" || (depth === 0 && !turn)) && (depth < MAX_DEPTH || (t.type !== "multi" && t.type !== "toggle")),
  );
  const delta = turn ? (
    <>
      <code>{"{delta}"}</code> becomes the signed number of detents, e.g. <code>+2</code>.
    </>
  ) : undefined;
  const type = action.kind.case;

  return (
    <div className="sd-action" data-depth={depth}>
      <Field label="Action">
        {(id) => (
          <select
            id={id}
            className="sd-input"
            value={type ?? ""}
            onChange={(e) => onChange(newAction(e.currentTarget.value as ActionType))}
          >
            {type === undefined && <option value="">Choose…</option>}
            {types.map((t) => (
              <option key={t.type} value={t.type}>
                {t.label}
              </option>
            ))}
          </select>
        )}
      </Field>
      <ActionFields action={action} edit={edit} choices={choices} delta={delta} depth={depth} turn={turn} />
    </div>
  );
}

interface ActionFieldsProps {
  action: KeyAction;
  edit(fn: (draft: KeyAction) => void): void;
  choices: Choices;
  delta?: ReactNode;
  depth: number;
  turn: boolean;
}

/** The fields of the action's type: integrations and nesting here, controls in `ControlFields`. */
function ActionFields({ action, edit, ...context }: ActionFieldsProps) {
  const kind = action.kind;
  switch (kind.case) {
    case "http":
      return <HttpFields {...context} value={kind.value} set={(fn) => edit((d) => d.kind.case === "http" && fn(d.kind.value))} />;
    case "homeassistant":
      return (
        <HomeAssistantFields
          {...context}
          value={kind.value}
          set={(fn) => edit((d) => d.kind.case === "homeassistant" && fn(d.kind.value))}
        />
      );
    case "lyrion":
      return <LyrionFields {...context} value={kind.value} set={(fn) => edit((d) => d.kind.case === "lyrion" && fn(d.kind.value))} />;
    case "multi":
      return <MultiFields {...context} value={kind.value} set={(fn) => edit((d) => d.kind.case === "multi" && fn(d.kind.value))} />;
    case "toggle":
      return <ToggleFields {...context} value={kind.value} set={(fn) => edit((d) => d.kind.case === "toggle" && fn(d.kind.value))} />;
    default:
      return <ControlFields action={action} edit={edit} {...context} />;
  }
}

/** The fields of command, mode, profile, brightness and audio actions. */
function ControlFields({ action, edit, ...context }: ActionFieldsProps) {
  const kind = action.kind;
  switch (kind.case) {
    case "command":
      return <CommandFields {...context} value={kind.value} set={(fn) => edit((d) => d.kind.case === "command" && fn(d.kind.value))} />;
    case "mode":
      return <ModeFields {...context} value={kind.value} set={(fn) => edit((d) => d.kind.case === "mode" && fn(d.kind.value))} />;
    case "profile":
      return <ProfileFields {...context} value={kind.value} set={(fn) => edit((d) => d.kind.case === "profile" && fn(d.kind.value))} />;
    case "brightness":
      return (
        <BrightnessFields {...context} value={kind.value} set={(fn) => edit((d) => d.kind.case === "brightness" && fn(d.kind.value))} />
      );
    case "audio":
      return <AudioFields {...context} value={kind.value} set={(fn) => edit((d) => d.kind.case === "audio" && fn(d.kind.value))} />;
    case "assist":
      return <AssistFields {...context} value={kind.value} set={(fn) => edit((d) => d.kind.case === "assist" && fn(d.kind.value))} />;
    default:
      return null;
  }
}

function HttpFields({ value, set, delta }: PartProps<HttpAction>) {
  const headers = Object.entries(value.headers)
    .map(([k, v]) => `${k}: ${v}`)
    .join("\n");
  const parseHeaders = (text: string) => {
    const parsed: Record<string, string> = {};
    for (const line of text.split("\n")) {
      const at = line.indexOf(":");
      if (at > 0) parsed[line.slice(0, at).trim()] = line.slice(at + 1).trim();
    }
    return parsed;
  };
  return (
    <>
      <div className="sd-action__row">
        <Field label="Method">
          {(id) => (
            <select
              id={id}
              className="sd-input sd-action__method"
              value={value.method || "POST"}
              onChange={(e) => {
                const method = e.currentTarget.value;
                set((v) => (v.method = method));
              }}
            >
              {["GET", "POST", "PUT", "PATCH", "DELETE"].map((m) => (
                <option key={m}>{m}</option>
              ))}
            </select>
          )}
        </Field>
        <Text label="URL" type="url" help={delta} value={value.url} onChange={(url) => set((v) => (v.url = url))} />
      </div>
      <Text label="Body" rows={2} help="Sent as is; empty for none." value={value.body} onChange={(body) => set((v) => (v.body = body))} />
      <Text
        label="Headers"
        rows={2}
        help="One per line: Name: value"
        value={headers}
        onChange={(text) => set((v) => (v.headers = parseHeaders(text)))}
      />
    </>
  );
}

function HomeAssistantFields({ value, set, choices }: PartProps<HomeAssistantAction>) {
  return (
    <>
      {!choices.homeassistant && <p className="sd-setting__error">Needs the Home Assistant module (Settings → Lights).</p>}
      <Text
        label="Service"
        help="domain.service, e.g. light.toggle, scene.turn_on, switch.turn_off"
        value={value.service}
        onChange={(service) => set((v) => (v.service = service))}
      />
      <Text
        label="Data (JSON)"
        rows={2}
        placeholder={'{"entity_id": "light.desk"}'}
        help={'e.g. {"entity_id": "light.desk", "brightness_pct": 60}'}
        value={value.dataJson}
        onChange={(data) => set((v) => (v.dataJson = data))}
      />
    </>
  );
}

function LyrionFields({ value, set, choices, delta }: PartProps<LyrionAction>) {
  return (
    <>
      {!choices.lyrion && <p className="sd-setting__error">Needs the Lyrion module (Settings → Music).</p>}
      <Text
        label="Player (optional)"
        help="Name or MAC; empty for the Lyrion module's player."
        value={value.player}
        onChange={(player) => set((v) => (v.player = player))}
      />
      <Text
        label="Command"
        rows={3}
        help={<>One word per line, e.g. mixer / volume / +5. {delta}</>}
        value={value.command.join("\n")}
        onChange={(text) => set((v) => (v.command = lines(text)))}
      />
    </>
  );
}

function CommandFields({ value, set, choices, delta }: PartProps<CommandAction>) {
  return (
    <>
      {!choices.allowCommands && (
        <p className="sd-setting__error">Commands are off. Turn on "Allow commands" for these keys first.</p>
      )}
      <Text
        label="Program and arguments"
        rows={3}
        help={<>One per line, no shell. {delta}</>}
        value={value.argv.join("\n")}
        onChange={(text) => set((v) => (v.argv = lines(text)))}
      />
    </>
  );
}

function ModeFields({ value, set, choices }: PartProps<ModeAction>) {
  return <Choice label="Mode" value={value.id} options={choices.modes} onChange={(id) => set((v) => (v.id = id))} />;
}

function ProfileFields({ value, set, choices }: PartProps<ProfileAction>) {
  return (
    <Choice
      label="Profile"
      value={value.name}
      options={choices.profiles}
      help={choices.profiles.length === 0 ? "No profiles are configured yet." : undefined}
      onChange={(name) => set((v) => (v.name = name))}
    />
  );
}

/** This machine's audio output (H15-50): volume step per detent, or mute toggle. */
/** Push-to-talk (H15-43): hold to talk, or tap and the end of speech ends it (H15-69). */
function AssistFields({ value, set }: PartProps<AssistAction>) {
  return (
    <Field
      label="Mode"
      help={
        <>
          {value.tap
            ? "Tap and speak; it ends when you stop talking. Tap again to end early, or while it answers to cancel."
            : "Hold the key while you speak."}{" "}
          Home Assistant answers on the key and through the speaker; settings in <code>[modules.homeassistant.assist]</code>.
        </>
      }
    >
      {(id) => (
        <select
          id={id}
          className="sd-input"
          value={value.tap ? "tap" : "hold"}
          onChange={(e) => {
            const tap = e.currentTarget.value === "tap";
            set((v) => (v.tap = tap));
          }}
        >
          <option value="hold">Hold to talk</option>
          <option value="tap">Tap to talk (ends by itself)</option>
        </select>
      )}
    </Field>
  );
}

function AudioFields({ value, set }: PartProps<AudioAction>) {
  const step = value.change.case !== "toggleMute";
  return (
    <div className="sd-action__row">
      <Field label="Change">
        {(id) => (
          <select
            id={id}
            className="sd-input"
            value={step ? "step" : "mute"}
            onChange={(e) => {
              const toStep = e.currentTarget.value === "step";
              set((v) => (v.change = toStep ? { case: "step", value: 2 } : { case: "toggleMute", value: true }));
            }}
          >
            <option value="step">Volume step (per detent)</option>
            <option value="mute">Mute / unmute</option>
          </select>
        )}
      </Field>
      {step && (
        <Field label="%">
          {(id) => (
            <input
              id={id}
              className="sd-input"
              type="number"
              min={-100}
              max={100}
              value={value.change.case === "step" ? value.change.value : 2}
              onChange={(e) => {
                const n = Number(e.currentTarget.value);
                set((v) => (v.change = { case: "step", value: n }));
              }}
            />
          )}
        </Field>
      )}
    </div>
  );
}

function BrightnessFields({ value, set }: PartProps<BrightnessAction>) {
  const step = value.change.case === "step";
  return (
    <div className="sd-action__row">
      <Field label="Change">
        {(id) => (
          <select
            id={id}
            className="sd-input"
            value={step ? "step" : "set"}
            onChange={(e) => {
              const toStep = e.currentTarget.value === "step";
              set((v) => (v.change = toStep ? { case: "step", value: 5 } : { case: "set", value: 80 }));
            }}
          >
            <option value="set">Set to</option>
            <option value="step">Step by (per detent)</option>
          </select>
        )}
      </Field>
      <Field label="%">
        {(id) => (
          <input
            id={id}
            className="sd-input"
            type="number"
            min={step ? -100 : 0}
            max={100}
            value={value.change.value ?? 0}
            onChange={(e) => {
              const n = Number(e.currentTarget.value);
              set((v) => (v.change = v.change.case === "step" ? { case: "step", value: n } : { case: "set", value: n }));
            }}
          />
        )}
      </Field>
    </div>
  );
}

function MultiFields({ value, set, choices, depth, delta }: PartProps<MultiAction>) {
  const count = value.steps.length;
  const move = (i: number, by: number) =>
    set((v) => {
      const [moved] = v.steps.splice(i, 1);
      if (moved) v.steps.splice(i + by, 0, moved);
    });
  return (
    <div className="sd-action__steps">
      {value.steps.map(({ step }, i) => (
        <div key={i} className="sd-action__step">
          <div className="sd-action__step-head">
            <span className="sd-setting__label">Step {i + 1}</span>
            <span className="sd-action__step-tools">
              <Button size="sm" disabled={i === 0} onClick={() => move(i, -1)}>
                ↑
              </Button>
              <Button size="sm" disabled={i === count - 1} onClick={() => move(i, 1)}>
                ↓
              </Button>
              <Button size="sm" onClick={() => set((v) => v.steps.splice(i, 1))}>
                Remove
              </Button>
            </span>
          </div>
          {step.case === "delayMs" ? (
            <Field label="Wait (ms)">
              {(id) => (
                <input
                  id={id}
                  className="sd-input"
                  type="number"
                  min={0}
                  max={60000}
                  value={step.value}
                  onChange={(e) => {
                    const ms = Number(e.currentTarget.value);
                    set((v) => (v.steps[i]!.step = { case: "delayMs", value: ms }));
                  }}
                />
              )}
            </Field>
          ) : (
            <ActionEditor
              action={step.value ?? newAction("http")}
              choices={choices}
              turn={delta !== undefined}
              depth={depth + 1}
              onChange={(next) => set((v) => (v.steps[i]!.step = { case: "action", value: next }))}
            />
          )}
        </div>
      ))}
      <div className="sd-settings__actions">
        <Button
          size="sm"
          onClick={() => set((v) => v.steps.push(create(MultiStepSchema, { step: { case: "action", value: newAction("http") } })))}
        >
          Add action
        </Button>
        <Button size="sm" onClick={() => set((v) => v.steps.push(create(MultiStepSchema, { step: { case: "delayMs", value: 500 } })))}>
          Add wait
        </Button>
      </div>
    </div>
  );
}

function ToggleFields({ value, set, choices, depth }: PartProps<ToggleAction>) {
  return (
    <div className="sd-action__toggle">
      <ToggleSide side="on" value={value} set={set} choices={choices} depth={depth} />
      <ToggleSide side="off" value={value} set={set} choices={choices} depth={depth} />
      <Text
        label="Follow entity"
        help="A Home Assistant entity the key follows, e.g. light.desk. Empty: the one both sides switch, if any."
        value={value.state}
        onChange={(state) => set((v) => (v.state = state))}
      />
    </div>
  );
}

function ToggleSide({ side, value, set, choices, depth }: PartProps<ToggleAction> & { side: "on" | "off" }) {
  const labelKey = side === "on" ? "onLabel" : "offLabel";
  const imageKey = side === "on" ? "onImage" : "offImage";
  return (
    <fieldset className="sd-action__side">
      <legend className="sd-setting__label">{side === "on" ? "Turn on (when off)" : "Turn off (when on)"}</legend>
      <div className="sd-action__row">
        <Text
          label="Label"
          placeholder="the key's label"
          value={value[labelKey]}
          onChange={(text) => set((v) => (v[labelKey] = text))}
        />
        <Field label="Image">
          {(id) => (
            <ImagePicker
              id={id}
              images={choices.images}
              icons={choices.icons}
              value={value[imageKey]}
              placeholder="the key's image"
              onChange={(name) => set((v) => (v[imageKey] = name))}
            />
          )}
        </Field>
      </div>
      <ActionEditor
        action={value[side] ?? newAction("homeassistant")}
        choices={choices}
        depth={depth + 1}
        onChange={(next) => set((v) => (v[side] = next))}
      />
    </fieldset>
  );
}
