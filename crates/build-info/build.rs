//! Build provenance without a build dependency or a requirement for git in source archives.
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let result = Command::new("git").args(args).current_dir(root).output().ok()?;
    result
        .status
        .success()
        .then(|| String::from_utf8_lossy(&result.stdout).trim().to_owned())
}

/// The web page of a commit, `{commit}` standing for its SHA: `STREAMDOCKD_COMMIT_URL` if set
/// (empty = no link), else derived from the `origin` remote (`<repo>/commit/{commit}`, as GitHub,
/// Forgejo and Gitea serve it), else none.
fn commit_url(root: &Path) -> String {
    if let Ok(url) = std::env::var("STREAMDOCKD_COMMIT_URL") {
        return url;
    }
    git(root, &["remote", "get-url", "origin"])
        .and_then(|remote| web_url(&remote))
        .map(|repo| format!("{repo}/commit/{{commit}}"))
        .unwrap_or_default()
}

/// `ssh://git@host/a/b.git`, `git@host:a/b.git` or `https://host/a/b.git` → `https://host/a/b`.
fn web_url(remote: &str) -> Option<String> {
    let rest = remote.strip_suffix(".git").unwrap_or(remote);
    let (host, path) = if let Some(url) = rest.strip_prefix("https://") {
        url.split_once('/')?
    } else if let Some(url) = rest.strip_prefix("ssh://") {
        let url = url.split_once('@').map_or(url, |(_, u)| u);
        let (host, path) = url.split_once('/')?;
        (host.split_once(':').map_or(host, |(h, _)| h), path)
    } else {
        let url = rest.split_once('@').map_or(rest, |(_, u)| u);
        url.split_once(':')?
    };
    (!host.is_empty() && !path.is_empty()).then(|| format!("https://{host}/{path}"))
}

fn main() {
    emit(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").as_path());
}

fn emit(root: &Path) {
    let commit = git(root, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_owned());
    let short = git(root, &["rev-parse", "--short=10", "HEAD"]).unwrap_or_else(|| "unknown".to_owned());
    let dirty = git(root, &["status", "--porcelain", "--untracked-files=normal"])
        .is_some_and(|status| !status.is_empty());
    let revision = if dirty { format!("{short}-dirty") } else { short };
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    println!("cargo::rustc-env=STREAMDOCK_BUILD_COMMIT={commit}");
    println!("cargo::rustc-env=STREAMDOCK_BUILD_REVISION={revision}");
    println!("cargo::rustc-env=STREAMDOCK_BUILD_DIRTY={dirty}");
    println!("cargo::rustc-env=STREAMDOCK_BUILD_TIME={seconds}");
    println!(
        "cargo::rustc-env=STREAMDOCK_BUILD_COMMIT_URL={}",
        commit_url(root)
    );
    println!("cargo::rerun-if-env-changed=STREAMDOCKD_COMMIT_URL");
    // Worktrees keep HEAD/index separately from the shared refs. Watch both, including packed refs.
    let mut refs = vec!["HEAD".to_owned(), "index".to_owned(), "packed-refs".to_owned()];
    if let Some(branch) = git(root, &["symbolic-ref", "-q", "HEAD"]) {
        refs.push(branch);
    }
    for name in &refs {
        if let Some(path) = git(
            root,
            &["rev-parse", "--path-format=absolute", "--git-path", name.as_str()],
        ) {
            println!("cargo::rerun-if-changed={path}");
        }
    }
    // Track source edits as well as commits, so an incremental build updates the dirty marker.
    if let Some(files) = git(root, &["ls-files"]) {
        for file in files.lines() {
            println!("cargo::rerun-if-changed={}", root.join(file).display());
        }
    }
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed={}", root.join("CHANGELOG.md").display());
}
