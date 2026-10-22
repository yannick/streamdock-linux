//! H15-80 step 2: what the transport to PipeWire costs, with a **silent** 48 kHz stereo f32 stream
//! (10 ms writes), so it can run next to real playback without being heard.
//!
//! - `pwcat`: through `player::output::PwCat` (a `pw-cat` child fed by a pipe, as today);
//! - `pulse`: the `PulseAudio` protocol to `pipewire-pulse` (pure Rust `pulseaudio` crate, data over
//!   the socket, clocked by the server's requests).
//!
//! Reports CPU (`/proc`, 100 ticks/s) of this process, its `pw-cat` child and `pipewire-pulse`.
//!
//! ```sh
//! sudo -u streamdockd -g sound env PIPEWIRE_RUNTIME_DIR=/run/user/1001 XDG_RUNTIME_DIR=/run/user/1001 \
//!   ./output_transport_probe pwcat 60
//! ```

use std::ffi::CString;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use player::output::{Device, PwCat};
use pulseaudio::protocol;

/// `(utime + stime)` ticks of a process.
fn ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some(f[11].parse::<u64>().ok()? + f[12].parse::<u64>().ok()?)
}

/// Processes named `name` (and, with `parent`, only children of it).
fn pids(name: &str, parent: Option<u32>) -> Vec<u32> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let comm = &stat[open + 1..close];
        let parent_pid: u32 = stat[close + 2..]
            .split_whitespace()
            .nth(1)
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        if comm == name && parent.is_none_or(|p| p == parent_pid) {
            out.push(pid);
        }
    }
    out
}

fn sum(pids: &[u32]) -> u64 {
    pids.iter().filter_map(|&p| ticks(p)).sum()
}

struct Meter {
    me: u32,
    start: Instant,
    t0: (u64, u64, u64),
}

impl Meter {
    fn start() -> Self {
        let me = std::process::id();
        let t0 = (
            sum(&[me]),
            sum(&pids("pw-cat", Some(me))),
            sum(&pids("pipewire-pulse", None)),
        );
        Self {
            me,
            start: Instant::now(),
            t0,
        }
    }

    #[allow(clippy::cast_precision_loss, reason = "H15-80: report")]
    fn report(&self, label: &str, children_t0: u64) {
        let secs = self.start.elapsed().as_secs_f64();
        let me = sum(&[self.me]) - self.t0.0;
        let children = pids("pw-cat", Some(self.me));
        let child = sum(&children).saturating_sub(children_t0);
        let pulse = sum(&pids("pipewire-pulse", None)) - self.t0.2;
        let pct = |t: u64| t as f64 / secs;
        println!(
            "{label:6} {secs:.0} s: probe {:.2} %, pw-cat {:.2} % ({} found), pipewire-pulse {:.2} %, total {:.2} %",
            pct(me),
            pct(child),
            children.len(),
            pct(pulse),
            pct(me + child + pulse)
        );
    }
}

const RATE: u32 = 48_000;
const BLOCK: usize = 480;

fn pwcat(seconds: u64) {
    let env: Vec<(String, String)> = ["PIPEWIRE_RUNTIME_DIR", "PIPEWIRE_CONFIG_NAME"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
        .collect();
    let mut dev = PwCat::new("pw-cat", env, Arc::new(|| None), Duration::from_millis(100));
    dev.open(RATE, 2).expect("pw-cat");
    // Let pw-cat start and connect before measuring.
    std::thread::sleep(Duration::from_secs(2));
    let meter = Meter::start();
    let child_t0 = sum(&pids("pw-cat", Some(meter.me)));
    let mut silence = vec![0.0f32; BLOCK * 2];
    while meter.start.elapsed() < Duration::from_secs(seconds) {
        // Blocks while the pipe is full: pw-cat clocks us.
        dev.write_frames(&mut silence).expect("write");
    }
    meter.report("pwcat", child_t0);
    dev.close();
}

fn pulse(seconds: u64) -> anyhow::Result<()> {
    let path = pulseaudio::socket_path_from_env().ok_or_else(|| anyhow::anyhow!("no PulseAudio socket"))?;
    let mut sock = BufReader::new(UnixStream::connect(path)?);
    let cookie = pulseaudio::cookie_path_from_env()
        .and_then(|p| std::fs::read(p).ok())
        .unwrap_or_default();
    protocol::write_command_message(
        sock.get_mut(),
        0,
        &protocol::Command::Auth(protocol::AuthParams {
            version: protocol::MAX_VERSION,
            supports_shm: false,
            supports_memfd: false,
            cookie,
        }),
        protocol::MAX_VERSION,
    )?;
    let (_, auth) = protocol::read_reply_message::<protocol::AuthReply>(&mut sock, protocol::MAX_VERSION)?;
    let version = protocol::MAX_VERSION.min(auth.version);
    let mut props = protocol::Props::new();
    props.set(
        protocol::Prop::ApplicationName,
        CString::new("streamdockd-probe").unwrap(),
    );
    protocol::write_command_message(
        sock.get_mut(),
        1,
        &protocol::Command::SetClientName(props),
        version,
    )?;
    let _ = protocol::read_reply_message::<protocol::SetClientNameReply>(&mut sock, version)?;
    let bytes_per_10ms = u32::try_from(BLOCK * 2 * 4).unwrap();
    // Smallest request, in 10 ms units (`MINREQ_10MS`, default 1): bigger means fewer messages.
    let min_request: u32 = std::env::var("MINREQ_10MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    protocol::write_command_message(
        sock.get_mut(),
        2,
        &protocol::Command::CreatePlaybackStream(protocol::PlaybackStreamParams {
            sample_spec: protocol::SampleSpec {
                format: protocol::SampleFormat::Float32Le,
                channels: 2,
                sample_rate: RATE,
            },
            channel_map: protocol::ChannelMap::stereo(),
            cvolume: Some(protocol::ChannelVolume::norm(2)),
            sink_name: Some(protocol::DEFAULT_SINK.to_owned()),
            buffer_attr: protocol::stream::BufferAttr {
                max_length: u32::MAX,
                target_length: bytes_per_10ms * 10.max(min_request * 2), // 100 ms, as pw-cat's --latency
                pre_buffering: u32::MAX,
                minimum_request_length: bytes_per_10ms * min_request,
                fragment_size: u32::MAX,
            },
            ..Default::default()
        }),
        version,
    )?;
    let (_, stream) =
        protocol::read_reply_message::<protocol::CreatePlaybackStreamReply>(&mut sock, version)?;
    let silence = vec![0u8; 1 << 16];
    let send = |sock: &mut BufReader<UnixStream>, n: u32| -> anyhow::Result<()> {
        let mut left = n as usize;
        while left > 0 {
            let k = left.min(silence.len());
            protocol::write_memblock(sock.get_mut(), stream.channel, &silence[..k], 0)?;
            left -= k;
        }
        Ok(())
    };
    send(&mut sock, stream.requested_bytes)?;
    let mut meter: Option<Meter> = None;
    let warmup = Instant::now();
    loop {
        let (_, msg) = protocol::read_command_message(&mut sock, version)?;
        match msg {
            protocol::Command::Request(r) if r.channel == stream.channel => send(&mut sock, r.length)?,
            protocol::Command::Underflow(_) => eprintln!("underflow"),
            protocol::Command::Error(e) => anyhow::bail!("server error: {e:?}"),
            _ => {}
        }
        if meter.is_none() && warmup.elapsed() > Duration::from_secs(2) {
            meter = Some(Meter::start());
        }
        if let Some(m) = &meter
            && m.start.elapsed() >= Duration::from_secs(seconds)
        {
            m.report("pulse", 0);
            return Ok(());
        }
    }
}

fn main() -> anyhow::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let seconds = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(60);
    match mode.as_str() {
        "pwcat" => pwcat(seconds),
        "pulse" => pulse(seconds)?,
        _ => anyhow::bail!("usage: output_transport_probe pwcat|pulse [seconds]"),
    }
    Ok(())
}
