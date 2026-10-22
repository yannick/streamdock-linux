// Editing on the dashboard (H15-32): the tab bar (add, rename by double click, reorder, delete,
// convert) and the right-click / long-press menus on the dock's keys, buttons and knobs. Every
// change goes to the daemon, which edits config.toml and reloads; the new state arrives through
// the WatchEvents stream like any other change.
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";

import { deviceClient } from "../api/client.js";
import {
  Button,
  ContextMenu,
  Dialog,
  InlineEdit,
  Stepper,
  useContextMenuTrigger,
  type ContextMenuItem,
  type ContextMenuState,
} from "../design/index.js";
import type { GetLightsTabResponse, HomeAssistantEntity, Mode, Screen } from "../gen/streamdock/v1/device_pb.js";
import { SlotEditor, pageModeId, useKeyPages } from "../keys/index.js";
import { ChoiceSelect } from "../settings/ChoiceSelect.js";
import { MusicPlayerDialog, useChoiceOptions } from "./MusicPlayer.js";
import { LightsActionEditor } from "./LightsActionEditor.js";
import { NowPlayingDialog } from "./nowPlaying.js";

function message(err: unknown): string {
  return err instanceof Error ? err.message.replace(/^\[\w+\]\s*/, "") : String(err);
}

const KIND_NAMES: Record<string, string> = { lights: "Lights", music: "Music", keys: "Keys" };

/** Kinds that can be renamed, deleted and moved (demo tabs come from [modules.demo]). */
const editable = (mode: Mode | undefined) => mode !== undefined && mode.kind in KIND_NAMES;

// ---------- Errors ----------

/** One line under the tab bar for the last failed change. */
function useErrorLine(): [ReactNode, (p: Promise<unknown>) => Promise<boolean>] {
  const [error, setError] = useState<string | null>(null);
  const run = useCallback(async (p: Promise<unknown>) => {
    try {
      await p;
      setError(null);
      return true;
    } catch (err) {
      setError(message(err));
      return false;
    }
  }, []);
  return [error ? <p className="sd-field__error" role="alert">{error}</p> : null, run];
}

// ---------- Add tab ----------

function AddTabDialog({
  kind,
  screen,
  onClose,
  run,
}: {
  kind: string | null;
  screen: Screen;
  onClose(): void;
  run(p: Promise<unknown>): Promise<boolean>;
}) {
  const [title, setTitle] = useState("");
  const [area, setArea] = useState("");
  const [player, setPlayer] = useState("");
  useEffect(() => {
    setTitle(kind ? KIND_NAMES[kind] : "");
    setArea("");
    setPlayer("");
  }, [kind]);
  const areas = useChoiceOptions("homeassistant", "area", kind === "lights");
  const players = useChoiceOptions("lyrion", "player", kind === "music");

  const add = async () => {
    if (!kind) return;
    let created = "";
    const ok = await run(
      deviceClient
        .addTab({ kind, title, area, player, profile: screen.activeProfile })
        .then((res) => {
          created = res.mode;
        }),
    );
    if (ok) {
      onClose();
      // Show the new tab once the reload has published it.
      setTimeout(() => void deviceClient.setMode({ mode: created }).catch(() => {}), 800);
    }
  };

  return (
    <Dialog
      open={kind !== null}
      title={`New ${kind ? KIND_NAMES[kind].toLowerCase() : ""} tab`}
      onClose={onClose}
      actions={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={!title.trim()} onClick={() => void add()}>
            Add
          </Button>
        </>
      }
    >
      <label className="sd-setting">
        <span className="sd-setting__label">Name</span>
        <input className="sd-input" value={title} onChange={(e) => setTitle(e.currentTarget.value)} autoFocus />
      </label>
      {kind === "lights" && (
        <label className="sd-setting">
          <span className="sd-setting__label">Area</span>
          <select className="sd-input" value={area} onChange={(e) => setArea(e.currentTarget.value)}>
            <option value="">None: choose an entity per key (right-click a key)</option>
            {areas.map((a) => (
              <option key={a.value}>{a.value}</option>
            ))}
          </select>
          <span className="sd-setting__help">The area's lights fill the keys; you can pin other entities per key later.</span>
        </label>
      )}
      {kind === "music" && (
        <label className="sd-setting">
          <span className="sd-setting__label">Player</span>
          <ChoiceSelect label="Player" options={players} value={player} onChange={setPlayer} emptyLabel="The default player" />
        </label>
      )}
      {kind === "keys" && (
        <p className="sd-setting__help">An empty page of custom keys. Right-click a key or knob to give it an action.</p>
      )}
    </Dialog>
  );
}

// ---------- Tab bar ----------

function Tab({
  mode,
  active,
  renaming,
  onRenaming,
  onMenu,
  run,
}: {
  mode: Mode;
  active: boolean;
  renaming: boolean;
  onRenaming(on: boolean): void;
  onMenu(x: number, y: number): void;
  run(p: Promise<unknown>): Promise<boolean>;
}) {
  const trigger = useContextMenuTrigger(onMenu);
  return (
    <button
      type="button"
      role="tab"
      className="sd-tabs__tab"
      aria-selected={active}
      data-kind={mode.kind}
      onClick={() => void run(deviceClient.setMode({ mode: mode.id }))}
      onDoubleClick={() => editable(mode) && onRenaming(true)}
      {...trigger}
    >
      {editable(mode) ? (
        <InlineEdit
          label={`Rename ${mode.title}`}
          value={mode.title}
          editing={renaming}
          onEditingChange={onRenaming}
          onSave={(title) => void run(deviceClient.renameTab({ mode: mode.id, title }))}
        />
      ) : (
        mode.title
      )}
    </button>
  );
}

/** The profile, the tabs (= modes) and the add button; knob 3 on the dock or a click here. */
export function TabBar({ screen }: { screen: Screen }) {
  const [errorLine, run] = useErrorLine();
  const [menu, setMenu] = useState<ContextMenuState | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [adding, setAdding] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<Mode | null>(null);
  const [choosingPlayer, setChoosingPlayer] = useState<Mode | null>(null);
  const [nowPlaying, setNowPlaying] = useState<Mode | null>(null);
  const closeMenu = useCallback(() => setMenu(null), []);
  const addTrigger = useContextMenuTrigger((x, y) => openAdd(x, y));

  const ids = screen.modes.map((m) => m.id);
  const move = (id: string, by: number) => {
    const order = [...ids];
    const at = order.indexOf(id);
    const to = at + by;
    if (at < 0 || to < 0 || to >= order.length) return;
    [order[at], order[to]] = [order[to], order[at]];
    void run(deviceClient.moveTabs({ order, profile: screen.activeProfile }));
  };

  const tabMenu = (mode: Mode, x: number, y: number) => {
    const at = ids.indexOf(mode.id);
    const items: ContextMenuItem[] = [
      { label: "Rename", disabled: !editable(mode), onSelect: () => setRenaming(mode.id) },
      { label: "Move left", disabled: at <= 0, onSelect: () => move(mode.id, -1) },
      { label: "Move right", disabled: at >= ids.length - 1, onSelect: () => move(mode.id, 1) },
    ];
    if (mode.kind === "music") {
      items.push({ label: "Player…", hint: "which Lyrion player", onSelect: () => setChoosingPlayer(mode) });
      items.push({ label: "Now playing…", hint: "band on the dock", onSelect: () => setNowPlaying(mode) });
    }
    if (mode.kind === "lights" || mode.kind === "music") {
      items.push({
        label: "Convert to custom keys",
        hint: "edit every key",
        onSelect: () => void run(deviceClient.convertTab({ mode: mode.id })),
      });
    }
    items.push({ label: "Delete…", disabled: !editable(mode), onSelect: () => setDeleting(mode) });
    setMenu({ x, y, title: `${KIND_NAMES[mode.kind] ?? "Tab"} · ${mode.title}`, items });
  };

  const openAdd = (x: number, y: number) =>
    setMenu({
      x,
      y,
      title: "Add a tab",
      items: Object.entries(KIND_NAMES).map(([kind, name]) => ({
        label: name,
        hint: { lights: "an area or chosen entities", music: "a Lyrion player", keys: "custom actions" }[kind],
        onSelect: () => setAdding(kind),
      })),
    });

  return (
    <div className="sd-modes">
      {screen.profiles.length > 0 && (
        <Stepper
          label="Profile"
          options={screen.profiles}
          value={screen.activeProfile}
          onChange={(profile) => void run(deviceClient.setProfile({ profile }))}
        />
      )}
      <div className="sd-tabs" role="tablist" aria-label="Mode">
        <kbd className="sd-keycap">K3</kbd>
        {screen.modes.map((mode) => (
          <Tab
            key={mode.id}
            mode={mode}
            active={mode.id === screen.activeMode}
            renaming={renaming === mode.id}
            onRenaming={(on) => setRenaming(on ? mode.id : null)}
            onMenu={(x, y) => tabMenu(mode, x, y)}
            run={run}
          />
        ))}
        <button
          type="button"
          className="sd-tabs__tab sd-tabs__add"
          aria-label="Add a tab"
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            openAdd(r.left, r.bottom + 4);
          }}
          {...addTrigger}
        >
          +
        </button>
      </div>
      {errorLine}
      <ContextMenu menu={menu} onClose={closeMenu} />
      <AddTabDialog kind={adding} screen={screen} onClose={() => setAdding(null)} run={run} />
      <MusicPlayerDialog mode={choosingPlayer} onClose={() => setChoosingPlayer(null)} run={run} />
      {nowPlaying && <NowPlayingDialog mode={nowPlaying} onClose={() => setNowPlaying(null)} run={run} />}
      <Dialog
        open={deleting !== null}
        title={`Delete ${deleting?.title ?? ""}?`}
        onClose={() => setDeleting(null)}
        actions={
          <>
            <Button onClick={() => setDeleting(null)}>Keep it</Button>
            <Button
              variant="primary"
              onClick={() => {
                if (deleting) void run(deviceClient.deleteTab({ mode: deleting.id }));
                setDeleting(null);
              }}
            >
              Delete
            </Button>
          </>
        }
      >
        <p>
          The tab and its settings are removed from the config. A backup (Settings → Backup) brings it back if
          needed.
        </p>
      </Dialog>
    </div>
  );
}

// ---------- Entity picker (lights tabs) ----------

const emptyPin = { entity: "", label: "", action: "", service: "", icon: "" };
function validPin(pin: typeof emptyPin): boolean {
  return !!pin.entity && (pin.action !== "service" || /^[a-z0-9_]+\.[a-z0-9_]+$/.test(pin.service));
}

function EntityDialog({
  open,
  mode,
  slot,
  onClose,
  run,
}: {
  open: boolean;
  mode: Mode | undefined;
  slot: number;
  onClose(): void;
  run(p: Promise<unknown>): Promise<boolean>;
}) {
  const [entities, setEntities] = useState<HomeAssistantEntity[] | null>(null);
  const [tab, setTab] = useState<GetLightsTabResponse | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [pin, setPin] = useState(emptyPin);
  const { entity: chosen, label, action, service, icon } = pin;

  // By id: the Mode object is new on every screen update, which must not restart the load.
  const modeId = mode?.id;
  useEffect(() => {
    if (!open || !modeId) return;
    let live = true;
    setEntities(null);
    setLoadError(null);
    setQuery("");
    Promise.all([deviceClient.listHomeAssistantEntities({}), deviceClient.getLightsTab({ mode: modeId })])
      .then(([list, current]) => {
        if (!live) return;
        setEntities(list.entities);
        setTab(current);
        const now = current.keys.find((k) => k.key === slot);
        setPin(now ? { ...now, action: now.service ? "service" : now.action } : emptyPin);
      })
      .catch((err: unknown) => live && setLoadError(message(err)));
    return () => {
      live = false;
    };
  }, [open, modeId, slot]);

  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    return (entities ?? []).filter(
      (e) => !q || e.name.toLowerCase().includes(q) || e.entityId.includes(q) || e.area.toLowerCase().includes(q),
    );
  }, [entities, query]);

  const save = async () => {
    if (!mode) return;
    const request = { mode: mode.id, key: slot, entity: chosen, label,
      action: action === "service" ? "" : action, service, icon };
    if (await run(deviceClient.setLightsKey(request))) onClose();
  };

  return (
    <Dialog
      open={open}
      title={`Key ${slot} · ${mode?.title ?? ""}`}
      onClose={onClose}
      actions={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={!validPin(pin)} onClick={() => void save()}>
            Save
          </Button>
        </>
      }
    >
      {tab && !tab.pinned && tab.area && (
        <p className="sd-setting__help">
          This tab shows the lights of {tab.area}. Choosing an entity pins the current lights to their keys, so you
          can change each one.
        </p>
      )}
      {loadError && <p className="sd-field__error">{loadError}</p>}
      <input
        className="sd-input"
        placeholder="Search entities, areas…"
        aria-label="Search entities"
        value={query}
        onChange={(e) => setQuery(e.currentTarget.value)}
      />
      <div className="sd-picker" role="listbox" aria-label="Home Assistant entities">
        {entities === null && !loadError && <span className="sd-setting__help">Loading from Home Assistant…</span>}
        {shown.slice(0, 200).map((e) => (
          <button
            key={e.entityId}
            type="button"
            role="option"
            aria-selected={e.entityId === chosen}
            className="sd-picker__item"
            onClick={() => setPin((p) => p.entity === e.entityId ? p : { ...p, entity: e.entityId, action: "", service: "" })}
          >
            <span>{e.name}</span>
            <span className="sd-picker__meta">
              {e.area ? `${e.area} · ` : ""}
              {e.entityId}
            </span>
          </button>
        ))}
      </div>
      <LightsActionEditor entity={chosen} action={action} service={service}
        onChange={(nextAction, nextService) => setPin((p) => ({ ...p, action: nextAction, service: nextService }))} />
      <label className="sd-setting">
        <span className="sd-setting__label">Label on the key</span>
        <input
          className="sd-input"
          placeholder="the entity's name"
          value={label}
          onChange={(e) => setPin({ ...pin, label: e.currentTarget.value })}
        />
      </label>
    </Dialog>
  );
}

// ---------- Custom keys editor (keys tabs, H15-29) ----------

/** The slot editor for one key, button or knob of the keys tab `mode`, in a dialog. */
function KeysSlotDialog({ mode, slot, onClose }: { mode: Mode; slot: Slot; onClose(): void }) {
  const { data, error, reload } = useKeyPages();
  const page = keysPageIndex(data, mode.id);
  return (
    <Dialog open title={`${slotName(slot)} · ${mode.title}`} onClose={onClose}>
      {error && <p className="sd-field__error">{error}</p>}
      {!data && !error && <span className="sd-setting__help">Loading…</span>}
      {data && page < 0 && <p className="sd-field__error">This tab is not a custom keys page.</p>}
      {data && page >= 0 && (
        <SlotEditor
          data={data}
          page={page}
          slot={{ kind: slot.type === "knob" ? "knob" : "key", number: slot.n }}
          onSaved={() => {
            reload();
            onClose();
          }}
          onCancel={onClose}
        />
      )}
    </Dialog>
  );
}

/** Index of the keys page shown as tab `modeId`, or -1 (not loaded, or not a keys tab). */
function keysPageIndex(data: ReturnType<typeof useKeyPages>["data"], modeId: string): number {
  const pages = data?.pages;
  if (!pages) return -1;
  return pages.pages.findIndex((_, i) => pageModeId(pages, i) === modeId);
}

// ---------- Dock menus ----------

export type Slot = { type: "key" | "button" | "knob"; n: number };

const KNOB_FUNCTIONS: Array<[string, string]> = [
  ["brightness", "Brightness"],
  ["color_temp", "Color temperature"],
  ["none", "Nothing"],
];

const SLOT_NAMES: Record<Slot["type"], string> = { key: "Key", button: "Button", knob: "Knob" };

/** "Key 3", "Button 7", "Knob 1". */
function slotName(slot: Slot): string {
  return `${SLOT_NAMES[slot.type]} ${slot.n}`;
}

type Run = (p: Promise<unknown>) => Promise<boolean>;

/** What the dock menu of `slot` can do besides calling the API. */
interface SlotMenuActions {
  run: Run;
  /** Opens the custom keys editor for the slot. */
  edit(): void;
  /** Opens the entity picker for the (lights) key. */
  pick(): void;
}

const disabledItem = (label: string): ContextMenuItem => ({ label, disabled: true, onSelect: () => {} });

/** The dock menu's items for `slot` on the tab `mode`, by the tab's kind. */
async function slotMenuItems(mode: Mode, slot: Slot, actions: SlotMenuActions): Promise<ContextMenuItem[]> {
  if (slot.type === "knob" && slot.n === 3) return [disabledItem("Knob 3 switches tabs")];
  if (mode.kind === "keys") return [{ label: "Edit…", hint: "action, label, image", onSelect: actions.edit }];
  const convert: ContextMenuItem = {
    label: "Convert tab to custom keys…",
    hint: "then edit any key",
    onSelect: () => void actions.run(deviceClient.convertTab({ mode: mode.id })),
  };
  if (mode.kind === "lights") return [...(await lightsSlotItems(mode, slot, actions)), convert];
  if (mode.kind === "music") return [convert];
  return [disabledItem("Demo tabs can't be edited")];
}

/** A lights tab's own items: entity per key, function per knob (1 and 2); none for buttons. */
async function lightsSlotItems(mode: Mode, slot: Slot, actions: SlotMenuActions): Promise<ContextMenuItem[]> {
  if (slot.type === "key") {
    return [
      { label: "Choose entity…", onSelect: actions.pick },
      {
        label: "Clear key",
        onSelect: () => void actions.run(deviceClient.setLightsKey({ mode: mode.id, key: slot.n, entity: "", label: "" })),
      },
    ];
  }
  if (slot.type !== "knob") return [];
  const current = await currentKnobFunction(mode, slot.n);
  return KNOB_FUNCTIONS.map(([fn, label]) => ({
    label,
    hint: fn === current ? "current" : undefined,
    onSelect: () => void actions.run(deviceClient.setLightsKnob({ mode: mode.id, knob: slot.n, function: fn })),
  }));
}

/** The function of lights knob `knob` (1 or 2), or "" when the daemon can't tell. */
async function currentKnobFunction(mode: Mode, knob: number): Promise<string> {
  try {
    const tab = await deviceClient.getLightsTab({ mode: mode.id });
    return knob === 1 ? tab.knob1 : tab.knob2;
  } catch {
    // Offer the choices anyway.
    return "";
  }
}

/**
 * Right-click (long press) menus for the dock's keys, buttons and knobs, acting on the active tab.
 * `bind(slot)` returns the props for a key tile or knob; `ui` holds the menu and dialogs.
 */
export function useDockMenus(screen: Screen | null) {
  const [menu, setMenu] = useState<ContextMenuState | null>(null);
  const [editing, setEditing] = useState<{ mode: Mode; slot: Slot } | null>(null);
  const [picking, setPicking] = useState<number | null>(null);
  const [errorLine, run] = useErrorLine();
  const closeMenu = useCallback(() => setMenu(null), []);
  const mode = screen?.modes.find((m) => m.id === screen.activeMode);

  const open = async (slot: Slot, x: number, y: number) => {
    if (!screen || !mode) return;
    const items = await slotMenuItems(mode, slot, {
      run,
      edit: () => setEditing({ mode, slot }),
      pick: () => setPicking(slot.n),
    });
    setMenu({ x, y, title: `${slotName(slot)} · ${mode.title}`, items });
  };

  const ui = (
    <>
      {errorLine}
      <ContextMenu menu={menu} onClose={closeMenu} />
      <EntityDialog open={picking !== null} mode={mode} slot={picking ?? 1} onClose={() => setPicking(null)} run={run} />
      {editing && <KeysSlotDialog mode={editing.mode} slot={editing.slot} onClose={() => setEditing(null)} />}
    </>
  );
  return { open, ui };
}

/** Props that open the dock menu for `slot` (right click or long press). */
export function useSlotTrigger(slot: Slot, open: (slot: Slot, x: number, y: number) => void) {
  return useContextMenuTrigger((x, y) => open(slot, x, y));
}
