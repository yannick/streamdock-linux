//! Identity captured when this binary's source was built, independent of the web build.

/// Full commit SHA, or `unknown` for a source archive without git.
pub const COMMIT: &str = env!("STREAMDOCK_BUILD_COMMIT");
/// Ten-character commit SHA, with `-dirty` for local edits.
pub const REVISION: &str = env!("STREAMDOCK_BUILD_REVISION");
/// UTC Unix build timestamp in seconds.
pub const BUILT_AT: &str = env!("STREAMDOCK_BUILD_TIME");
/// Whether the source checkout contained edits when built.
pub const DIRTY: bool = matches!(env!("STREAMDOCK_BUILD_DIRTY").as_bytes(), b"true");
/// The commit's web page with `{commit}` for the SHA, or empty (see `build.rs`).
pub const COMMIT_URL: &str = env!("STREAMDOCK_BUILD_COMMIT_URL");
