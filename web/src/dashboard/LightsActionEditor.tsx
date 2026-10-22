/** Pinned entities keep their normal behaviour unless an explicit action is selected. */
export function LightsActionEditor({ entity, action, service, onChange }: {
  entity: string;
  action: string;
  service: string;
  onChange(action: string, service: string): void;
}) {
  const selected = service ? "service" : action;
  return (
    <>
      <label className="sd-setting">
        <span className="sd-setting__label">Action</span>
        <select className="sd-input" value={selected} onChange={(e) => {
          const value = e.currentTarget.value;
          onChange(value, "");
        }}>
          <option value="">Default (toggle or activate)</option>
          {entity.startsWith("cover.") && <>
            <option value="open">Open / up</option>
            <option value="stop">Stop</option>
            <option value="close">Close / down</option>
          </>}
          <option value="service">Custom service</option>
        </select>
      </label>
      {selected === "service" && <label className="sd-setting">
        <span className="sd-setting__label">Service</span>
        <input className="sd-input" placeholder="switch.turn_on" value={service}
          onChange={(e) => onChange("service", e.currentTarget.value)} />
        <span className="sd-setting__help">Targets only this entity, without extra data.</span>
      </label>}
      {["open", "close", "stop"].includes(action) && <p className="sd-setting__help">Shows the blind’s position. Press a direction while it moves to stop.</p>}
    </>
  );
}
