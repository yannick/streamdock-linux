//! The decoder's input: the HTTP body chunks from `slimproto`, read as a byte stream. Chunks are
//! pulled ahead of the decoder up to the input buffer size, so that much compressed audio is
//! buffered and reported as the decoder fill (`PlaybackStatus::decode_fill`).

use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use bytes::{Buf, Bytes};
use tokio::runtime::Runtime;
use tokio::sync::{Notify, mpsc};

/// Cancels one track: wakes its source if it waits for data, and every later read fails.
#[derive(Debug, Default)]
pub(super) struct Cancel {
    flag: AtomicBool,
    wake: Notify,
}

impl Cancel {
    pub(super) fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
        // `notify_one` keeps a permit when nobody waits yet, so a read that is about to wait
        // still sees it.
        self.wake.notify_one();
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

/// The error a read returns once its track was cancelled.
#[derive(Debug, thiserror::Error)]
#[error("track cancelled")]
pub(super) struct Cancelled;

/// A blocking [`Read`] over the body chunks. Runs on the decoder thread; `rt` (a current-thread
/// runtime of that thread) only waits for the next chunk or the cancel signal.
pub(super) struct StreamSource<'a> {
    rx: mpsc::Receiver<Bytes>,
    queue: VecDeque<Bytes>,
    queued: usize,
    capacity: usize,
    closed: bool,
    fill: &'a AtomicU32,
    cancel: &'a Cancel,
    rt: &'a Runtime,
}

impl<'a> StreamSource<'a> {
    pub(super) fn new(
        rx: mpsc::Receiver<Bytes>,
        capacity: usize,
        fill: &'a AtomicU32,
        cancel: &'a Cancel,
        rt: &'a Runtime,
    ) -> Self {
        Self {
            rx,
            queue: VecDeque::with_capacity(64),
            queued: 0,
            capacity,
            closed: false,
            fill,
            cancel,
            rt,
        }
    }

    fn push(&mut self, chunk: Bytes) {
        if !chunk.is_empty() {
            self.queued += chunk.len();
            self.queue.push_back(chunk);
        }
    }

    /// Takes every chunk already waiting, up to the buffer size, without blocking.
    fn pull(&mut self) {
        while !self.closed && self.queued < self.capacity {
            match self.rx.try_recv() {
                Ok(chunk) => self.push(chunk),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => self.closed = true,
            }
        }
    }

    /// Waits for the next chunk; `false` when cancelled.
    fn wait(&mut self) -> bool {
        let rx = &mut self.rx;
        let wake = &self.cancel.wake;
        let next = self.rt.block_on(async move {
            tokio::select! {
                biased;
                () = wake.notified() => None,
                chunk = rx.recv() => Some(chunk),
            }
        });
        match next {
            None => false,
            Some(Some(chunk)) => {
                self.push(chunk);
                true
            }
            Some(None) => {
                self.closed = true;
                true
            }
        }
    }

    fn report_fill(&self) {
        self.fill
            .store(u32::try_from(self.queued).unwrap_or(u32::MAX), Ordering::Relaxed);
    }
}

impl Read for StreamSource<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.cancel.is_cancelled() {
                return Err(io::Error::other(Cancelled));
            }
            self.pull();
            if let Some(front) = self.queue.front_mut() {
                let n = front.len().min(buf.len());
                buf[..n].copy_from_slice(&front[..n]);
                front.advance(n);
                if front.is_empty() {
                    self.queue.pop_front();
                }
                self.queued -= n;
                self.report_fill();
                return Ok(n);
            }
            if self.closed {
                return Ok(0);
            }
            if !self.wait() {
                return Err(io::Error::other(Cancelled));
            }
        }
    }
}

/// A live stream cannot seek; symphonia asks only when [`symphonia::core::io::MediaSource`] says
/// it can.
impl Seek for StreamSource<'_> {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "stream is not seekable",
        ))
    }
}

impl symphonia::core::io::MediaSource for StreamSource<'_> {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}
