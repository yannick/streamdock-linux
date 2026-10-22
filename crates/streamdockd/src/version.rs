//! The running binary's identity; never the checkout's current HEAD at request time.
use streamdock_proto::streamdock::v1::GetVersionResponse;

pub fn get() -> GetVersionResponse {
    let commit = build_info::COMMIT;
    GetVersionResponse {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        revision: build_info::REVISION.to_owned(),
        commit: commit.to_owned(),
        commit_url: if commit == "unknown" {
            String::new()
        } else {
            build_info::COMMIT_URL.replace("{commit}", commit)
        },
        built_at: build_info::BUILT_AT.parse().unwrap_or_default(),
        dirty: build_info::DIRTY,
        ..Default::default()
    }
}
