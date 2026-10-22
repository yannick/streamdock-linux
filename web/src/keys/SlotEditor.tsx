// Edits one key, button or knob of a custom keys page (H15-29) and saves it with `SaveKeyPages`.
// No dialog chrome of its own: the /keys page shows it in a panel, the dashboard (H15-32) in a dialog.
import { clone, create } from "@bufbuild/protobuf";
import { useEffect, useState } from "react";

import { Button } from "../design/index.js";
import {
  KeySlotSchema,
  KnobSlotSchema,
  type GetKeyPagesResponse,
  type KeyAction,
  type KeySlot,
  type KnobSlot,
} from "../gen/streamdock/v1/device_pb.js";
import { ActionEditor, Field } from "./ActionEditor.js";
import { ImagePicker } from "./ImagePicker.js";
import { choicesOf, errorText, findSlot, newAction, savePages, withSlot, type Choices, type SlotRef } from "./model.js";

/** The fields of a key or knob slot the form edits (a key has no turn/press, a knob no detail/image/action). */
interface SlotDraft {
  label: string;
  detail: string;
  image: string;
  action?: KeyAction;
  turn?: KeyAction;
  press?: KeyAction;
}

export interface SlotEditorProps {
  /** From `useKeyPages()`. */
  data: GetKeyPagesResponse;
  /** Index into `data.pages.pages`. */
  page: number;
  slot: SlotRef;
  /** After a successful save or removal; reload the pages here. `restart` = the daemon needs one. */
  onSaved(restart: boolean): void;
  onCancel?(): void;
}

function draftOf(data: GetKeyPagesResponse, page: number, slot: SlotRef): KeySlot | KnobSlot {
  const pages = data.pages;
  const found = pages ? findSlot(pages, page, slot) : undefined;
  if (slot.kind === "key") {
    return found ? clone(KeySlotSchema, found as KeySlot) : create(KeySlotSchema, { key: slot.number, action: newAction("http") });
  }
  return found
    ? clone(KnobSlotSchema, found as KnobSlot)
    : create(KnobSlotSchema, { knob: slot.number, turn: newAction("brightness") });
}

export function SlotEditor({ data, page, slot, onSaved, onCancel }: SlotEditorProps) {
  const [draft, setDraft] = useState<KeySlot | KnobSlot>(() => draftOf(data, page, slot));
  const [status, setStatus] = useState<Status>(null);
  const [busy, setBusy] = useState(false);
  // A different slot, or fresh data after a save, starts a fresh draft; the save result stays shown
  // until another slot is picked. (Callers keep `slot` stable while it stays selected.)
  useEffect(() => {
    setDraft(draftOf(data, page, slot));
  }, [data, page, slot]);
  useEffect(() => {
    setStatus(null);
  }, [page, slot]);

  const pages = data.pages;
  if (!pages || !pages.pages[page]) return <p className="sd-setting__error">No such page.</p>;
  const choices = choicesOf(data);

  const save = (value: KeySlot | KnobSlot | null) => {
    setBusy(true);
    setStatus(null);
    savePages(withSlot(pages, page, slot, value))
      .then((restart) => {
        setStatus({ ok: true, text: restart ? "Saved. Restart streamdockd to use it." : "Saved and applied." });
        onSaved(restart);
      })
      .catch((err: unknown) => setStatus({ ok: false, text: errorText(err) }))
      .finally(() => setBusy(false));
  };
  const update = (fn: (d: SlotDraft) => void) => {
    const next = slot.kind === "key" ? clone(KeySlotSchema, draft as KeySlot) : clone(KnobSlotSchema, draft as KnobSlot);
    fn(next as unknown as SlotDraft);
    setDraft(next);
  };

  return (
    <div className="sd-slot">
      <h3 className="sd-slot__title">{slotTitle(slot)}</h3>
      <SlotFields slot={slot} draft={draft} choices={choices} update={update} />
      {status && <p className={status.ok ? "sd-setting__ok" : "sd-setting__error"}>{status.text}</p>}
      <div className="sd-settings__actions">
        <Button variant="primary" disabled={busy} onClick={() => save(draft)}>
          Save
        </Button>
        {findSlot(pages, page, slot) !== undefined && (
          <Button disabled={busy} onClick={() => save(null)}>
            Remove
          </Button>
        )}
        {onCancel && (
          <Button disabled={busy} onClick={onCancel}>
            Cancel
          </Button>
        )}
      </div>
    </div>
  );
}

type Status = { ok: boolean; text: string } | null;

function slotTitle(slot: SlotRef): string {
  if (slot.kind === "knob") return `Knob ${slot.number}`;
  return slot.number <= 6 ? `Key ${slot.number}` : `Button ${slot.number}`;
}

interface SlotFieldsProps {
  slot: SlotRef;
  draft: KeySlot | KnobSlot;
  choices: Choices;
  update(fn: (d: SlotDraft) => void): void;
}

/** Label, second line and image (LCD keys only), then the action, or a knob's turn and press. */
function SlotFields({ slot, draft, choices, update }: SlotFieldsProps) {
  if (slot.kind === "knob") {
    const knob = draft as KnobSlot;
    return (
      <>
        <LabelField value={knob.label} update={update} />
        {(["turn", "press"] as const).map((part) => (
          <KnobPart key={part} part={part} action={knob[part]} choices={choices} onChange={(next) => update((d) => (d[part] = next))} />
        ))}
      </>
    );
  }
  const key = draft as KeySlot;
  const lcd = slot.number <= 6;
  return (
    <>
      <div className="sd-action__row">
        <LabelField value={key.label} update={update} help={lcd ? undefined : "Buttons have no screen; the label is for you."} />
        {lcd && <TextField label="Second line" value={key.detail} onChange={(detail) => update((d) => (d.detail = detail))} />}
      </div>
      {lcd && (
        <Field label="Image" help="A built-in icon, or your own picture (upload on Settings → Key images).">
          {(id) => (
            <ImagePicker id={id} images={choices.images} icons={choices.icons} value={key.image} onChange={(name) => update((d) => (d.image = name))} />
          )}
        </Field>
      )}
      <ActionEditor action={key.action ?? newAction("http")} choices={choices} onChange={(next) => update((d) => (d.action = next))} />
    </>
  );
}

function LabelField({ value, update, help }: { value: string; update(fn: (d: SlotDraft) => void): void; help?: string }) {
  return <TextField label="Label" value={value} help={help} onChange={(label) => update((d) => (d.label = label))} />;
}

function TextField({ label, value, onChange, help }: { label: string; value: string; onChange(v: string): void; help?: string }) {
  return (
    <Field label={label} help={help}>
      {(id) => <input id={id} className="sd-input" value={value} onChange={(e) => onChange(e.currentTarget.value)} />}
    </Field>
  );
}

function KnobPart({
  part,
  action,
  choices,
  onChange,
}: {
  part: "turn" | "press";
  action: KeyAction | undefined;
  choices: Choices;
  onChange(next: KeyAction | undefined): void;
}) {
  return (
    <fieldset className="sd-action__side">
      <legend className="sd-setting__label">{part === "turn" ? "Turn" : "Press"}</legend>
      {action ? (
        <>
          <ActionEditor action={action} choices={choices} turn={part === "turn"} onChange={onChange} />
          <Button size="sm" onClick={() => onChange(undefined)}>
            No {part} action
          </Button>
        </>
      ) : (
        <Button size="sm" onClick={() => onChange(newAction(part === "turn" ? "brightness" : "http"))}>
          Add a {part} action
        </Button>
      )}
    </fieldset>
  );
}
