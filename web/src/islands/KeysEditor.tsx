// The custom keys page (H15-29), at /keys: pages (modes) of keys and knobs, laid out like the dock.
// Pick a key, button or knob to edit it with the `SlotEditor`; page settings save on their own.
import { clone, create } from "@bufbuild/protobuf";
import { useEffect, useState } from "react";

import { Button, KeyTile, Panel, Tabs, Toggle } from "../design/index.js";
import {
  KeyPageSchema,
  KeyPagesSchema,
  type GetKeyPagesResponse,
  type KeyPage,
  type KeyPages,
} from "../gen/streamdock/v1/device_pb.js";
import { errorText, pageModeId, savePages, SlotEditor, useKeyPages, type SlotRef } from "../keys/index.js";

type Status = { ok: boolean; text: string } | null;

export default function KeysEditor() {
  const { data, error, reload, incoming, accept } = useKeyPages();
  if (error) {
    return (
      <Panel title="Custom keys">
        <p className="sd-setting__error">{error}</p>
      </Panel>
    );
  }
  if (!data) return <Panel title="Custom keys">Loading…</Panel>;
  return <Editor data={data} reload={reload} incoming={incoming !== null} accept={accept} />;
}

interface EditorProps {
  data: GetKeyPagesResponse;
  reload(): void;
  /** The keys changed elsewhere (H15-49); `accept` loads them. */
  incoming: boolean;
  accept(): void;
}

function Editor({ data, reload, incoming, accept }: EditorProps) {
  const saved = data.pages ?? create(KeyPagesSchema);
  const [page, setPage] = useState(0);
  const [slot, setSlot] = useState<SlotRef | null>(null);
  // Page settings (enabled, titles, ids, the page list) are edited on a copy and saved together.
  const [draft, setDraft] = useState<KeyPages>(() => clone(KeyPagesSchema, saved));
  useEffect(() => {
    setDraft(clone(KeyPagesSchema, data.pages ?? create(KeyPagesSchema)));
  }, [data]);

  const editing = slot !== null || changed(draft, saved);

  const current = draft.pages[page];
  // Slots can only be edited on pages the daemon already has (a new page is saved first).
  const pageSaved = page < saved.pages.length;
  const choosePage = (next: number) => {
    setPage(next);
    setSlot(null);
  };

  return (
    <div className="sd-settings sd-keys">
      <Incoming incoming={incoming} editing={editing} accept={accept} />
      <PagesPanel draft={draft} saved={saved} page={page} onDraft={setDraft} onPage={choosePage} onSaved={reload} />
      {current && (
        <Panel title={current.title || pageModeId(draft, page)} aside="pick a slot">
          {pageSaved ? (
            <SlotGrid page={saved.pages[page]} slot={slot} onPick={setSlot} />
          ) : (
            <p className="sd-settings__help">Save the new page first, then give its keys actions.</p>
          )}
        </Panel>
      )}
      {current && pageSaved && slot && (
        <Panel title="Edit" className="sd-keys__editor">
          <SlotEditor data={data} page={page} slot={slot} onSaved={() => reload()} onCancel={() => setSlot(null)} />
        </Panel>
      )}
    </div>
  );
}

/** A change elsewhere is taken at once, unless the user is editing: then it waits for a click. */
function Incoming({ incoming, editing, accept }: { incoming: boolean; editing: boolean; accept(): void }) {
  useEffect(() => {
    if (incoming && !editing) accept();
  }, [incoming, editing, accept]);
  if (!incoming || !editing) return null;
  return (
    <div className="sd-trial" role="alert">
      <strong>The keys were changed elsewhere.</strong> Load them? Your unsaved changes here are lost.
      <Button variant="primary" size="sm" onClick={accept}>
        Load
      </Button>
    </div>
  );
}

/** Whether the page settings differ from what the daemon has. */
function changed(draft: KeyPages, saved: KeyPages): boolean {
  const titles = (p: KeyPages) => JSON.stringify(p.pages.map((x) => [x.id, x.title]));
  return (
    titles(draft) !== titles(saved) || draft.enabled !== saved.enabled || draft.allowCommands !== saved.allowCommands
  );
}

interface PagesPanelProps {
  draft: KeyPages;
  saved: KeyPages;
  page: number;
  onDraft(next: KeyPages): void;
  onPage(page: number): void;
  onSaved(): void;
}

/** The module switches, the page tabs and the selected page's title and id. */
function PagesPanel({ draft, saved, page, onDraft, onPage, onSaved }: PagesPanelProps) {
  const [status, setStatus] = useState<Status>(null);
  const current = draft.pages[page];
  const edit = (fn: (d: KeyPages) => void) => {
    const next = clone(KeyPagesSchema, draft);
    fn(next);
    onDraft(next);
    setStatus(null);
  };
  const save = () => {
    savePages(draft)
      .then((restart) => {
        setStatus({ ok: true, text: restart ? "Saved. Restart streamdockd to use it." : "Saved and applied." });
        onSaved();
      })
      .catch((err: unknown) => setStatus({ ok: false, text: errorText(err) }));
  };
  const addPage = () => {
    edit((d) => {
      d.enabled = true;
      d.pages.push(create(KeyPageSchema, { title: `Keys ${d.pages.length + 1}` }));
    });
    onPage(draft.pages.length);
  };
  const deletePage = () => {
    edit((d) => d.pages.splice(page, 1));
    onPage(Math.max(0, page - 1));
  };
  const count = `${draft.pages.length} page${draft.pages.length === 1 ? "" : "s"}`;

  return (
    <Panel title="Custom keys" aside={count}>
      <p className="sd-settings__help">
        Each page is a mode on the dock (knob 3 switches). Pick a key, button or knob below to give it an action.
      </p>
      <ModuleSwitches draft={draft} edit={edit} />
      {draft.pages.length > 0 && (
        <Tabs
          label="Pages"
          tabs={draft.pages.map((p, i) => ({ id: String(i), label: p.title || pageModeId(draft, i) }))}
          value={String(page)}
          onChange={(id) => onPage(Number(id))}
        />
      )}
      {current && <PageFields draft={draft} page={page} edit={edit} />}
      {status && <p className={status.ok ? "sd-setting__ok" : "sd-setting__error"}>{status.text}</p>}
      <div className="sd-settings__actions">
        <Button variant="primary" disabled={!changed(draft, saved)} onClick={save}>
          Save pages
        </Button>
        <Button onClick={addPage}>Add page</Button>
        {current && <Button onClick={deletePage}>Delete page</Button>}
      </div>
    </Panel>
  );
}

function ModuleSwitches({ draft, edit }: { draft: KeyPages; edit(fn: (d: KeyPages) => void): void }) {
  return (
    <div className="sd-action__row">
      <div className="sd-setting">
        <span className="sd-setting__label">Keys module</span>
        <Toggle label="Keys module" checked={draft.enabled} onChange={(on) => edit((d) => (d.enabled = on))} />
      </div>
      <div className="sd-setting">
        <span className="sd-setting__label">Allow commands</span>
        <Toggle
          label="Allow commands"
          checked={draft.allowCommands}
          onChange={(on) => edit((d) => (d.allowCommands = on))}
        />
        <span className="sd-setting__help">Command actions run programs as the daemon's user.</span>
      </div>
    </div>
  );
}

function PageFields({ draft, page, edit }: { draft: KeyPages; page: number; edit(fn: (d: KeyPages) => void): void }) {
  const current = draft.pages[page]!;
  // The id the daemon gives this page when it has none of its own.
  const blank = create(KeyPagesSchema, { pages: draft.pages.map(() => create(KeyPageSchema)) });
  return (
    <div className="sd-action__row">
      <div className="sd-setting">
        <label className="sd-setting__label" htmlFor="page-title">
          Title
        </label>
        <input
          id="page-title"
          className="sd-input"
          value={current.title}
          onChange={(e) => {
            const title = e.currentTarget.value;
            edit((d) => (d.pages[page]!.title = title));
          }}
        />
      </div>
      <div className="sd-setting">
        <label className="sd-setting__label" htmlFor="page-id">
          Mode id
        </label>
        <input
          id="page-id"
          className="sd-input"
          placeholder={pageModeId(blank, page)}
          value={current.id}
          onChange={(e) => {
            const id = e.currentTarget.value;
            edit((d) => (d.pages[page]!.id = id));
          }}
        />
      </div>
    </div>
  );
}

/** What a slot shows in the grid: its label, image name or a dot; undefined when unset. */
function slotLabel(page: KeyPage | undefined, s: SlotRef): string | undefined {
  if (!page) return undefined;
  if (s.kind === "key") {
    const k = page.keys.find((k) => k.key === s.number);
    return k ? k.label || k.image || "•" : undefined;
  }
  const k = page.knobs.find((k) => k.knob === s.number);
  return k ? k.label || "•" : undefined;
}

/** The dock-shaped grid: keys 1-6, buttons 7-9 and knobs 1-2. */
function SlotGrid({ page, slot, onPick }: { page: KeyPage | undefined; slot: SlotRef | null; onPick(s: SlotRef): void }) {
  const selected = (s: SlotRef) => slot?.kind === s.kind && slot.number === s.number;
  return (
    <>
      <div className="sd-keys__grid">
        {[1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) => {
          const s: SlotRef = { kind: "key", number: n };
          return (
            <div key={n} className="sd-keys__cell" data-selected={selected(s) || undefined}>
              <KeyTile number={n} label={slotLabel(page, s) ?? (n <= 6 ? "—" : `B${n}`)} onClick={() => onPick(s)} />
            </div>
          );
        })}
      </div>
      <div className="sd-keys__knobs">
        {[1, 2].map((n) => {
          const s: SlotRef = { kind: "knob", number: n };
          return (
            <Button key={n} variant={selected(s) ? "primary" : "default"} onClick={() => onPick(s)}>
              Knob {n}: {slotLabel(page, s) ?? "—"}
            </Button>
          );
        })}
        <span className="sd-setting__help">Knob 3 switches modes.</span>
      </div>
    </>
  );
}
