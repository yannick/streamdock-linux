//! Best-effort realtime scheduling for the blocking audio writer (H15-62).
//! Only this thread is promoted; `RESET_ON_FORK` keeps helper children at normal priority.

use std::io;
use std::process::Command;

fn current_tid() -> io::Result<u32> {
    let link = std::fs::read_link("/proc/thread-self")?;
    link.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.parse().ok())
        .ok_or_else(|| io::Error::other("invalid /proc/thread-self thread id"))
}

pub(super) fn promote() {
    match set_priority() {
        Ok(()) => tracing::info!(priority = 10, "player output realtime scheduling enabled"),
        Err(error) => tracing::warn!(%error, "player output uses normal scheduling"),
    }
}

fn set_priority() -> io::Result<()> {
    let tid = current_tid()?;
    let output = Command::new("chrt")
        .args(["--fifo", "--reset-on-fork", "--pid", "10"])
        .arg(tid.to_string())
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn resolves_the_calling_worker_instead_of_the_process_leader() {
        let worker = std::thread::spawn(|| {
            let tid = super::current_tid().unwrap();
            assert_ne!(tid, std::process::id());
            assert!(std::path::Path::new(&format!("/proc/self/task/{tid}")).exists());
        });
        worker.join().unwrap();
    }
}
