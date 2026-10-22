//! Demo module for development and tests: LCD keys toggle on and off, knob 1 counts, knob 2
//! press resets. `[modules.demo] pages = N` creates N independent demo modes.

use anyhow::Context as _;

use super::{FaceState, Input, Module, ModuleEvent, ModuleIo, ModuleTask, Page, face, knob};

/// Builds the demo modules from `[modules.demo]`.
pub fn from_table(table: &toml::Table) -> anyhow::Result<Vec<Box<dyn Module>>> {
    let pages = match table.get("pages") {
        None => 1,
        Some(v) => v
            .as_integer()
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| (1..=9).contains(n))
            .context("[modules.demo] pages must be 1-9")?,
    };
    Ok((1..=pages)
        .map(|n| {
            Box::new(Demo {
                id: format!("demo-{n}"),
                title: format!("Demo {n}"),
            }) as Box<dyn Module>
        })
        .collect())
}

#[derive(Debug)]
pub struct Demo {
    id: String,
    title: String,
}

#[derive(Debug, Default)]
struct State {
    on: [bool; 6],
    count: i32,
}

impl State {
    fn page(&self, title: &str) -> Page {
        let mut faces: Vec<_> = (1..=6u8)
            .map(|k| {
                let on = self.on[usize::from(k - 1)];
                let state = if on {
                    FaceState::FACE_STATE_ON
                } else {
                    FaceState::FACE_STATE_IDLE
                };
                face(k, format!("{title} {k}"), if on { "on" } else { "off" }, state)
            })
            .collect();
        faces[0].detail = format!("count {}", self.count);
        Page {
            now_playing: None,
            faces,
            knobs: vec![knob(1, "Count"), knob(2, "Reset")],
        }
    }
}

impl Module for Demo {
    fn id(&self) -> &str {
        &self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn kind(&self) -> &'static str {
        "demo"
    }

    fn run(self: Box<Self>, mut io: ModuleIo) -> ModuleTask {
        Box::pin(async move {
            let mut state = State::default();
            io.page.send_replace(state.page(&self.title));
            while let Some(event) = io.events.recv().await {
                match event {
                    ModuleEvent::Input(Input::Key {
                        key: k @ 1..=6,
                        pressed: true,
                    }) => {
                        let on = &mut state.on[usize::from(k - 1)];
                        *on = !*on;
                        let what = if *on { "on" } else { "off" };
                        io.activity.info(format!("{} {k} {what}", self.title));
                    }
                    ModuleEvent::Input(Input::KnobTurn { knob: 1, delta }) => state.count += delta,
                    ModuleEvent::Input(Input::KnobPress {
                        knob: 2,
                        pressed: true,
                    }) => state.count = 0,
                    _ => continue,
                }
                io.page.send_replace(state.page(&self.title));
            }
        })
    }
}
