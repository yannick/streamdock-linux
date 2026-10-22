// Living style guide for the design system (H15-11), served at /design. Every token and component
// is shown here with working state, so changes can be reviewed on desktop and phone.
import { useEffect, useState, type CSSProperties, type ReactNode } from "react";

import {
  Badge,
  Button,
  DataList,
  Glitch,
  HintBar,
  KeyTile,
  Keycap,
  Disclosure,
  Knob,
  MenuList,
  MenuRow,
  Meter,
  Panel,
  Readout,
  SectionTitle,
  Slider,
  Stepper,
  Tabs,
  Toggle,
  accents,
  color,
  colorRoles,
  cssVar,
  cut,
  duration,
  fontSize,
  space,
  type Accent,
  NowPlaying,
} from "../design/index.js";

type Section = "tokens" | "controls" | "display" | "dock";

const SECTIONS: ReadonlyArray<{ id: Section; label: string }> = [
  { id: "tokens", label: "Tokens" },
  { id: "controls", label: "Controls" },
  { id: "display", label: "Display" },
  { id: "dock", label: "Dock" },
];

const DENSITY = ["Low", "Medium", "High"] as const;

export default function DesignSystem() {
  const [accent, setAccent] = useState<Accent>("cyan");
  const [section, setSection] = useState<Section>("tokens");

  useEffect(() => {
    // The theme attribute lives on <html> so the page shell follows too.
    if (accent === "cyan") delete document.documentElement.dataset.sdAccent;
    else document.documentElement.dataset.sdAccent = accent;
  }, [accent]);

  return (
    <div className="sd-stack" style={{ "--sd-gap": cssVar("space-5") } as CSSProperties}>
      <div className="sd-cluster" style={{ justifyContent: "space-between" }}>
        <h2 className="sd-title">
          <Glitch text="Design system" />
        </h2>
        <div className="sd-cluster" role="group" aria-label="Accent theme">
          <span className="sd-label">Accent</span>
          {accents.map((a) => (
            <Button key={a} size="sm" aria-pressed={a === accent} onClick={() => setAccent(a)}>
              {a}
            </Button>
          ))}
        </div>
      </div>

      <Tabs label="Style guide sections" tabs={SECTIONS} value={section} onChange={setSection} prevHint="1" nextHint="3" />

      {section === "tokens" && <TokensSection />}
      {section === "controls" && <ControlsSection />}
      {section === "display" && <DisplaySection />}
      {section === "dock" && <DockSection />}

      <HintBar
        hints={[
          { key: "ESC", label: "Close" },
          { key: "F1", label: "Restore defaults" },
          { key: "↵", label: "Select" },
        ]}
      />
    </div>
  );
}

function Swatch({ name, value, note }: { name: string; value: string; note?: string }) {
  return (
    <div className="sd-cluster" style={{ "--sd-gap": cssVar("space-2"), flexWrap: "nowrap" } as CSSProperties}>
      <span
        aria-hidden
        style={{ flex: "none", width: 36, height: 36, background: value, border: `1px solid ${cssVar("color-line-faint")}` }}
      />
      <span style={{ minWidth: 0 }}>
        <span className="sd-mono" style={{ fontSize: cssVar("font-size-sm"), overflowWrap: "anywhere" }}>
          {name}
        </span>
        {note !== undefined && (
          <span className="sd-label" style={{ display: "block" }}>
            {note}
          </span>
        )}
      </span>
    </div>
  );
}

function Grid({ min = 220, children }: { min?: number; children: ReactNode }) {
  return (
    <div className="sd-grid" style={{ "--sd-grid-min": `${min}px` } as CSSProperties}>
      {children}
    </div>
  );
}

function TokensSection() {
  return (
    <div className="sd-stack">
      <Panel title="Semantic colors" aside="--sd-color-*">
        <Grid min={160}>
          {colorRoles.map((role) => (
            <Swatch key={role} name={role} value={cssVar(`color-${role}`)} />
          ))}
        </Grid>
      </Panel>

      <Panel title="Primitives" aside="no red, by design">
        <div className="sd-stack">
          {Object.entries(color).map(([hue, steps]) => (
            <div key={hue}>
              <SectionTitle>{hue}</SectionTitle>
              <Grid min={150}>
                {Object.entries(steps).map(([step, hex]) => (
                  <Swatch key={step} name={`${hue}-${step}`} value={hex} note={hex} />
                ))}
              </Grid>
            </div>
          ))}
        </div>
      </Panel>

      <Grid min={300}>
        <Panel title="Type scale" aside="SD Condensed / Fira Mono">
          <div className="sd-stack" style={{ "--sd-gap": cssVar("space-2") } as CSSProperties}>
            {Object.entries(fontSize).map(([k, v]) => (
              <div key={k} className="sd-cluster" style={{ flexWrap: "nowrap" }}>
                <span className="sd-label" style={{ width: 40 }}>
                  {k}
                </span>
                <span style={{ fontSize: v, lineHeight: 1.1, textTransform: "uppercase", letterSpacing: cssVar("tracking-wide") }}>
                  Netrunner
                </span>
              </div>
            ))}
          </div>
        </Panel>

        <Panel title="Space, cut, motion">
          <div className="sd-stack" style={{ "--sd-gap": cssVar("space-2") } as CSSProperties}>
            {Object.entries(space).map(([k, v]) => (
              <div key={k} className="sd-cluster" style={{ flexWrap: "nowrap" }}>
                <span className="sd-label" style={{ width: 64 }}>
                  space-{k}
                </span>
                <span style={{ width: v, height: 10, background: cssVar("color-accent") }} />
                <span className="sd-mono sd-label">{v}px</span>
              </div>
            ))}
            <SectionTitle>Chamfer</SectionTitle>
            <div className="sd-cluster">
              {Object.entries(cut).map(([k, v]) => (
                <div
                  key={k}
                  className="sd-frame"
                  style={{ "--sd-cut": `${v}px`, width: 72, height: 48, display: "grid", placeItems: "center" } as CSSProperties}
                >
                  <span className="sd-label">{k}</span>
                </div>
              ))}
            </div>
            <SectionTitle>Motion</SectionTitle>
            <p className="sd-label" style={{ margin: 0 }}>
              {Object.entries(duration)
                .map(([k, v]) => `${k} ${v} ms`)
                .join(" · ")}
              , eased; all zero with reduced motion.
            </p>
          </div>
        </Panel>
      </Grid>
    </div>
  );
}

function ControlsSection() {
  const [tutorials, setTutorials] = useState(true);
  const [analytics, setAnalytics] = useState(false);
  const [density, setDensity] = useState<(typeof DENSITY)[number]>("High");
  const [brightness, setBrightness] = useState(80);
  // onCommit fires once on release: what a dashboard would send as SetBrightness.
  const [committed, setCommitted] = useState(80);
  const [selected, setSelected] = useState(0);

  return (
    <Grid min={320}>
      <Panel title="Settings list" aside="toggle · stepper · slider" accent>
        <SectionTitle>Miscellaneous</SectionTitle>
        <MenuList label="Settings">
          <MenuRow label="Tutorials" selected={selected === 0} onSelect={() => setSelected(0)}>
            <Toggle label="Tutorials" checked={tutorials} onChange={setTutorials} />
          </MenuRow>
          <MenuRow label="Enable analytics" selected={selected === 1} onSelect={() => setSelected(1)}>
            <Toggle label="Enable analytics" checked={analytics} onChange={setAnalytics} />
          </MenuRow>
          <MenuRow label="Crowd density" selected={selected === 2} onSelect={() => setSelected(2)}>
            <Stepper label="Crowd density" options={DENSITY} value={density} onChange={setDensity} />
          </MenuRow>
          <MenuRow label={`Brightness ${brightness}% (sent ${committed}%)`} selected={selected === 3} onSelect={() => setSelected(3)}>
            <Slider label="Brightness" value={brightness} onChange={setBrightness} onCommit={setCommitted} />
          </MenuRow>
        </MenuList>
      </Panel>

      <Panel title="Buttons">
        <div className="sd-stack">
          <div className="sd-cluster">
            <Button variant="primary">Defaults</Button>
            <Button>Gamma correction</Button>
            <Button hint="X">Control scheme</Button>
          </div>
          <div className="sd-cluster">
            <Button variant="ghost">Ghost</Button>
            <Button disabled>Disabled</Button>
          </div>
          <SectionTitle>Badges</SectionTitle>
          <div className="sd-cluster">
            <Badge>Equipped</Badge>
            <Badge tone="success">Online</Badge>
            <Badge tone="warning">Low signal</Badge>
            <Badge tone="info">Legendary</Badge>
            <Badge tone="critical">Critical</Badge>
            <Badge tone="muted">Offline</Badge>
          </div>
          <SectionTitle>Keycaps</SectionTitle>
          <div className="sd-cluster">
            <Keycap>1</Keycap>
            <Keycap>3</Keycap>
            <Keycap>ESC</Keycap>
            <Keycap>F1</Keycap>
          </div>
        </div>
      </Panel>
    </Grid>
  );
}

function DisplaySection() {
  return (
    <Grid min={300}>
      <Panel title="Readouts">
        <div className="sd-stack">
          <div className="sd-cluster" style={{ "--sd-gap": cssVar("space-6") } as CSSProperties}>
            <Readout value="003" label="Level" size="lg" />
            <Readout value="27" label="Street cred" />
            <Readout value="80" unit="%" label="Brightness" size="sm" />
          </div>
          <SectionTitle>Meters</SectionTitle>
          <Meter label="Health" value={0.82} />
          <Meter label="RAM" value={0.45} tone="success" />
          <Meter label="Heat" value={0.7} tone="warning" />
          <Meter label="Integrity" value={0.18} tone="critical" />
        </div>
      </Panel>

      <Panel title="Now playing" aside="H15-88">
        <div className="sd-stack">
          <SectionTitle>Short</SectionTitle>
          <NowPlaying text="Teardrop · Massive Attack" playing durationMs={330000} elapsedMs={95000} elapsedAtMs={Date.now()} />
          <SectionTitle>Long, scrolling</SectionTitle>
          <NowPlaying
            text="Kapitel 20 - Folge 10: Die Nebelinsel · Die Nebelbande · Folge 10: Die Nebelinsel"
            playing
            durationMs={120000}
            elapsedMs={30000}
            elapsedAtMs={Date.now()}
          />
          <SectionTitle>Paused</SectionTitle>
          <NowPlaying text="Kapitel 20 - Folge 10: Die Nebelinsel · Die Nebelbande" paused durationMs={120000} elapsedMs={64000} />
          <SectionTitle>Nothing playing · offline</SectionTitle>
          <NowPlaying text="" idle />
          <NowPlaying text="" offline />
          <SectionTitle>Dock band (3 keys)</SectionTitle>
          <div className="sd-dock__lcd">
            {[0, 1, 2].map((index) => (
              <KeyTile
                key={index}
                number={index + 1}
                label={
                  <NowPlaying
                    text="Kapitel 20 - Folge 10: Die Nebelinsel · Die Nebelbande"
                    playing
                    durationMs={120000}
                    elapsedMs={30000}
                    elapsedAtMs={Date.now()}
                    slice={{ index, count: 3 }}
                  />
                }
              />
            ))}
          </div>
        </div>
      </Panel>

      <VolumeKeyPanel />
      <LightLevelsPanel />

      <Panel title="Executive summary" aside="DB-001">
        <DataList
          items={[
            { term: "Name", value: "StreamDock N3" },
            { term: "Codename", value: "AKP03 | 6603:1003" },
            { term: "Firmware", value: "V25.293N3_E.01.004" },
            { term: "Last position held", value: "Desk, left of keyboard" },
          ]}
        />
      </Panel>

      <Panel title="Inline data list">
        <DataList
          inline
          items={[
            { term: "Prevalence", value: "Unknown" },
            { term: "Hazard index", value: "5/5/10" },
            { term: "Notes", value: "N/A" },
          ]}
        />
        <Disclosure title="Disclosure" aside="folded">
          <p>Long dialogs fold their less used sections.</p>
        </Disclosure>
        <div className="sd-hazard" style={{ marginTop: cssVar("space-4"), padding: cssVar("space-3") }}>
          <span className="sd-label" style={{ color: cssVar("color-critical") }}>
            Hazard fill: critical states without red
          </span>
        </div>
      </Panel>
    </Grid>
  );
}

/** A key as the dock renders it (`/design/face.png`, the daemon's renderer). */
function faceUrl(params: Record<string, string | number>): string {
  return `/design/face.png?${new URLSearchParams(Object.entries(params).map(([k, v]) => [k, String(v)])).toString()}`;
}

const SAMPLE_TRACK = "Kapitel 20 - Folge 10: Die Nebelinsel · Die Nebelbande";

/** The volume on key 3 (H15-95): two ways to share it with the now-playing band. */
function VolumeKeyPanel() {
  const [level, setLevel] = useState(40);
  const band = (index: number, count: number) => faceUrl({ band: SAMPLE_TRACK, index, count, progress: 250 });
  const volume = faceUrl({ label: "Volume", level });
  const transport = ["previous", "pause", "next"];
  const rows: Array<{ title: string; keys: string[] }> = [
    { title: "A · band on 1–3, bar on 3 for ~3 s after each change", keys: [band(0, 3), band(1, 3), volume] },
    { title: "A · the rest of the time", keys: [band(0, 3), band(1, 3), band(2, 3)] },
    { title: "B · band on 1–2, bar on 3 all the time", keys: [band(0, 2), band(1, 2), volume] },
  ];
  return (
    <Panel title="Volume on key 3" aside="H15-95">
      <div className="sd-stack">
        <Slider label="Volume" value={level} onChange={setLevel} />
        {rows.map((row) => (
          <div key={row.title} className="sd-stack">
            <SectionTitle>{row.title}</SectionTitle>
            <div className="sd-dock__lcd">
              {row.keys.map((src, i) => (
                <KeyTile key={i} number={i + 1} image={src} />
              ))}
              {transport.map((icon, i) => (
                <KeyTile key={icon} number={i + 4} image={faceUrl({ icon, state: icon === "pause" ? "active" : "idle" })} />
              ))}
            </div>
          </div>
        ))}
      </div>
    </Panel>
  );
}

/** Brightness and colour temperature on keys 1–3 of a lights tab (H15-97). */
function LightLevelsPanel() {
  const [brightness, setBrightness] = useState(62);
  const [warmth, setWarmth] = useState(18);
  // A warm-white ceiling light: 2200–6500 K.
  const kelvin = Math.round((2200 + (4300 * warmth) / 100) / 50) * 50;
  const keys = [
    faceUrl({ label: "Decke", detail: `${brightness} %`, state: "on", icon: "light-on", level: brightness, style: "strip" }),
    faceUrl({ label: "Hell", level: brightness }),
    faceUrl({ label: "Farbe", level: warmth, detail: `${kelvin} K`, style: "temperature" }),
    faceUrl({ label: "Storen", icon: "blinds-up" }),
    faceUrl({ label: "Storen", icon: "blinds-stop" }),
    faceUrl({ label: "Storen", icon: "blinds-down" }),
  ];
  return (
    <Panel title="Light levels" aside="H15-97">
      <div className="sd-stack">
        <Slider label="Brightness" value={brightness} onChange={setBrightness} />
        <Slider label="Colour temperature" value={warmth} onChange={setWarmth} />
        <div className="sd-dock__lcd">
          {keys.map((src, i) => (
            <KeyTile key={i} number={i + 1} image={src} />
          ))}
        </div>
      </div>
    </Panel>
  );
}

function DockSection() {
  const [pressed, setPressed] = useState<number | null>(null);
  const [released, setReleased] = useState<{ key: number; pulse: number } | null>(null);
  const [knobs, setKnobs] = useState([0, 0, 0]);
  const [turn, setTurn] = useState<{ knob: number; dir: -1 | 1; pulse: number } | null>(null);
  const [knobDown, setKnobDown] = useState<number | null>(null);

  // A gentle demo loop so pressed, release-flash and turn states are visible without the dock.
  useEffect(() => {
    let tick = 0;
    let last: number | null = null;
    const id = setInterval(() => {
      tick += 1;
      const key = tick % 4 === 0 ? ((tick / 4) % 9) + 1 : null;
      if (key === null && last !== null) setReleased({ key: last, pulse: tick });
      last = key;
      setPressed(key);
      const knob = tick % 3;
      const dir: -1 | 1 = tick % 2 ? 1 : -1;
      setKnobs((k) => k.map((v, i) => v + (i === knob ? dir : 0)));
      setTurn({ knob, dir, pulse: tick });
      setKnobDown(tick % 6 === 0 ? (tick / 6) % 3 : null);
    }, 450);
    return () => clearInterval(id);
  }, []);

  return (
    <Panel title="StreamDock N3" aside="pressed · release flash (pulse) · turn (turn + pulse)" accent>
      <div className="sd-stack">
        <div style={{ display: "grid", gridTemplateColumns: "repeat(3, minmax(0, 1fr))", gap: cssVar("space-3"), maxWidth: 420 }}>
          {[1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) => (
            <KeyTile
              key={n}
              number={n}
              pressed={pressed === n}
              pulse={released?.key === n ? released.pulse : undefined}
              label={n <= 6 ? `Key ${n}` : undefined}
            />
          ))}
        </div>
        <div className="sd-cluster" style={{ "--sd-gap": cssVar("space-6") } as CSSProperties}>
          {[1, 2, 3].map((n) => (
            <Knob
              key={n}
              number={n}
              position={knobs[n - 1] ?? 0}
              pressed={knobDown === n - 1}
              turn={turn?.knob === n - 1 ? turn.dir : 0}
              pulse={turn?.knob === n - 1 ? turn.pulse : undefined}
              onTurn={(d) => {
                setKnobs((k) => k.map((v, i) => v + (i === n - 1 ? d : 0)));
                setTurn({ knob: n - 1, dir: d > 0 ? 1 : -1, pulse: Date.now() });
              }}
              onPress={() => setKnobDown(n - 1)}
            />
          ))}
        </div>
      </div>
    </Panel>
  );
}
