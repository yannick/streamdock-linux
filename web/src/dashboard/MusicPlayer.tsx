// Which Lyrion player a music tab controls (H15-87): the players the server announces, by name with
// their state, stored by MAC. Opened from the tab's menu ("Player…"); the add-tab dialog uses the
// same list.
import { useEffect, useState } from "react";

import { deviceClient } from "../api/client.js";
import { Button, Dialog } from "../design/index.js";
import type { Mode, SettingChoice } from "../gen/streamdock/v1/device_pb.js";
import { ChoiceSelect } from "../settings/ChoiceSelect.js";

/** Suggestions for a settings field (HA areas, Lyrion players), asked with the saved settings. */
export function useChoiceOptions(module: string, key: string, enabled: boolean): SettingChoice[] {
  const [options, setOptions] = useState<SettingChoice[]>([]);
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    deviceClient
      .getSettings({})
      .then((settings) => {
        const values = settings.modules.find((m) => m.id === module)?.values ?? {};
        return deviceClient.getSettingChoices({ module, key, values });
      })
      .then((res) => live && setOptions(res.options))
      .catch(() => live && setOptions([]));
    return () => {
      live = false;
    };
  }, [module, key, enabled]);
  return options;
}

interface MusicPlayerDialogProps {
  /** The music tab, or null when closed. */
  mode: Mode | null;
  onClose(): void;
  run(p: Promise<unknown>): Promise<boolean>;
}

export function MusicPlayerDialog({ mode, onClose, run }: MusicPlayerDialogProps) {
  const options = useChoiceOptions("lyrion", "player", mode !== null);
  const [player, setPlayer] = useState("");
  useEffect(() => {
    if (!mode) return;
    setPlayer("");
    deviceClient
      .getMusicTab({ mode: mode.id })
      .then((res) => setPlayer(res.effectivePlayer))
      .catch(() => setPlayer(""));
  }, [mode]);
  const save = async () => {
    if (!mode) return;
    if (await run(deviceClient.setMusicTabPlayer({ mode: mode.id, player }))) onClose();
  };
  return (
    <Dialog
      open={mode !== null}
      title={`Player for ${mode?.title ?? ""}`}
      onClose={onClose}
      actions={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={!player.trim()} onClick={() => void save()}>
            Use this player
          </Button>
        </>
      }
    >
      <label className="sd-setting">
        <span className="sd-setting__label">Lyrion player</span>
        {options.length > 0 ? (
          <ChoiceSelect label="Lyrion player" options={options} value={player} onChange={setPlayer} />
        ) : (
          <span className="sd-setting__help">Loading the players from Lyrion…</span>
        )}
        <span className="sd-setting__help">
          The tab's keys, knob 1 (volume) and buttons control this player. It is saved by its id, so a rename in
          Lyrion doesn't matter.
        </span>
      </label>
    </Dialog>
  );
}
