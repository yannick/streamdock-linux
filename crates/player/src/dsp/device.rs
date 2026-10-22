use super::{Compiled, Processor, Settings};
use crate::output::{CHUNK_FRAMES, Device};
use arc_swap::ArcSwap;
use std::{io, sync::Arc};

#[derive(Debug)]
struct Snapshot {
    version: u64,
    settings: Settings,
}

/// Control-plane publication; readers never take a mutex on the audio thread.
#[derive(Debug)]
pub struct Controls(ArcSwap<Snapshot>);
impl Controls {
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self(ArcSwap::from_pointee(Snapshot { version: 1, settings }))
    }
    pub fn publish(&self, settings: Settings) {
        self.0.rcu(|old| {
            if old.settings == settings {
                Arc::clone(old)
            } else {
                Arc::new(Snapshot {
                    version: old.version.wrapping_add(1),
                    settings,
                })
            }
        });
    }
}

/// Native DSP shared by music and Assist. `None` leaves unmanaged outputs untouched.
#[derive(Debug)]
pub struct DspDevice<D> {
    inner: D,
    controls: Option<Arc<Controls>>,
    processor: Option<Processor>,
    rate: u32,
    channels: u16,
    scratch: Vec<f32>,
    tail: bool,
}
impl<D: Device> DspDevice<D> {
    #[must_use]
    pub fn new(inner: D, controls: Option<Arc<Controls>>) -> Self {
        Self {
            inner,
            controls,
            processor: None,
            rate: 0,
            channels: 0,
            scratch: Vec::new(),
            tail: false,
        }
    }
    fn compiled(&self) -> io::Result<Option<Compiled>> {
        let Some(controls) = &self.controls else {
            return Ok(None);
        };
        // Exactly one guarded load per block, released before the blocking device write.
        let snapshot = controls.0.load();
        if self
            .processor
            .as_ref()
            .is_some_and(|p| p.version() == snapshot.version)
        {
            return Ok(None);
        }
        let mut settings = snapshot.settings;
        if self.channels == 1 {
            settings.crossfeed = None;
        }
        Compiled::new(self.rate, snapshot.version, settings).map(Some)
    }
}
impl<D: Device> Device for DspDevice<D> {
    fn open(&mut self, rate: u32, channels: u16) -> io::Result<()> {
        self.close();
        self.rate = rate;
        self.channels = channels;
        let output_channels = if self.controls.is_some() && channels == 1 {
            2
        } else {
            channels
        };
        if let Some(config) = self.compiled()? {
            self.processor = Some(Processor::new(usize::from(output_channels), &config)?);
        }
        self.scratch = vec![0.0; CHUNK_FRAMES * usize::from(output_channels.max(1))];
        self.inner.open(rate, output_channels)
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if !bytes.len().is_multiple_of(4) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "incomplete float sample",
            ));
        }
        // Compatibility route only; the player passes floats without this conversion.
        let mut frames: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        self.write_frames(&mut frames)
    }
    fn write_frames(&mut self, frames: &mut [f32]) -> io::Result<()> {
        if let Some(config) = self.compiled()? {
            tracing::debug!(
                rate = self.rate,
                version = config.version,
                "native DSP settings applied"
            );
            self.processor
                .as_mut()
                .ok_or_else(|| io::Error::other("DSP device closed"))?
                .update(&config)?;
        }
        let Some(processor) = &mut self.processor else {
            return self.inner.write_frames(frames);
        };
        if self.channels == 1 {
            for block in frames.chunks(CHUNK_FRAMES) {
                let scratch = &mut self.scratch[..block.len() * 2];
                for (pair, sample) in scratch.as_chunks_mut::<2>().0.iter_mut().zip(block) {
                    pair.fill(*sample);
                }
                processor.process(scratch)?;
                self.inner.write_frames(scratch)?;
            }
        } else {
            processor.process(frames)?;
            self.inner.write_frames(frames)?;
        }
        self.tail |= !frames.is_empty();
        Ok(())
    }
    fn delay_frames(&self) -> u32 {
        self.inner.delay_frames().saturating_add(
            self.processor
                .as_ref()
                .map_or(0, |p| u32::try_from(p.delay_frames()).unwrap_or(u32::MAX)),
        )
    }
    fn reset(&mut self) {
        if let Some(processor) = &mut self.processor {
            processor.reset();
        }
        self.tail = false;
        self.inner.reset();
    }
    fn close(&mut self) {
        if self.tail
            && let Some(processor) = &mut self.processor
        {
            let channels = if self.channels == 1 {
                2
            } else {
                usize::from(self.channels)
            };
            let mut remaining = processor.delay_frames();
            while remaining > 0 {
                let frames = remaining.min(CHUNK_FRAMES);
                let scratch = &mut self.scratch[..frames * channels];
                scratch.fill(0.0);
                if let Err(error) = processor
                    .process(scratch)
                    .and_then(|()| self.inner.write_frames(scratch))
                {
                    tracing::warn!(%error, "native DSP tail could not be written");
                    break;
                }
                remaining -= frames;
            }
        }
        self.processor = None;
        self.tail = false;
        self.inner.close();
    }
    fn is_open(&self) -> bool {
        self.inner.is_open()
    }
}
