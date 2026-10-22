//! Command-line arguments (H15-99). `streamdockd` takes its settings from the config file and the
//! environment; the only arguments are `--version` and `--help`. Both print and exit, and anything
//! else is an error, before any config is read, any port is bound or any dock is opened: only one
//! daemon may own a dock (opening it clears its keys), so a stray argument must never start a second one.

/// What `main` does.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// No arguments: run the daemon.
    Run,
    /// Print `text` (to stderr if `error`) and exit with `code`.
    Exit { text: String, code: i32, error: bool },
}

const USAGE: &str = "\
Usage: streamdockd [--version | --help]

Drives StreamDock docks and serves the web UI and API. Settings come from the config file and the
environment, not from arguments:

  STREAMDOCKD_CONFIG       config file (default ~/.config/streamdockd/config.toml)
  STREAMDOCKD_ADDR         listen address, e.g. 0.0.0.0:7315 (overrides `listen`)
  STREAMDOCKD_HARDWARE     0 = never open a real dock (for dev instances)
  STREAMDOCKD_FAKE_DEVICE  1 = also run the simulated dock
  STREAMDOCKD_FAKE_INPUT   1 = the simulated dock presses keys by itself (demo modes only)
  STREAMDOCKD_PLAYER_SILENT 1 = the built-in Lyrion player plays nothing (dev)
  RUST_LOG                 log level, e.g. info or streamdockd=debug

Only one streamdockd may own a dock: never start a second one next to the service.
";

/// The version line: the same as `GetVersion` (version, revision, build time).
#[must_use]
pub fn version_line() -> String {
    let v = crate::version::get();
    let dirty = if v.dirty { ", dirty" } else { "" };
    format!(
        "streamdockd {} ({}{dirty}, built {})",
        v.version, v.revision, v.built_at
    )
}

fn unexpected(what: &str) -> Action {
    Action::Exit {
        text: format!(
            "streamdockd: unexpected argument {what}; settings come from the config file and the \
             environment (see --help). Nothing was started."
        ),
        code: 2,
        error: true,
    }
}

/// Decides from the arguments (without the program name).
#[must_use]
pub fn parse(mut args: impl Iterator<Item = String>) -> Action {
    let Some(first) = args.next() else {
        return Action::Run;
    };
    let extra = args.next();
    match (first.as_str(), extra) {
        ("--version" | "-V", None) => Action::Exit {
            text: version_line(),
            code: 0,
            error: false,
        },
        ("--help" | "-h", None) => Action::Exit {
            text: USAGE.trim_end().to_owned(),
            code: 0,
            error: false,
        },
        (known @ ("--version" | "-V" | "--help" | "-h"), Some(extra)) => {
            unexpected(&format!("{extra:?} after {known}"))
        }
        (_, _) => unexpected(&format!("{first:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Action {
        parse(list.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn no_arguments_run_the_daemon() {
        assert_eq!(args(&[]), Action::Run);
    }

    #[test]
    fn version_and_help_print_and_exit_0() {
        let Action::Exit { text, code, error } = args(&["--version"]) else {
            panic!("--version must not run the daemon");
        };
        assert_eq!((code, error), (0, false));
        assert!(text.starts_with("streamdockd "), "{text}");
        assert_eq!(text, version_line());
        let Action::Exit { text, code, .. } = args(&["--help"]) else {
            panic!("--help must not run the daemon");
        };
        assert_eq!(code, 0);
        assert!(text.contains("STREAMDOCKD_CONFIG") && text.contains("STREAMDOCKD_HARDWARE"));
        assert!(matches!(args(&["-V"]), Action::Exit { code: 0, .. }));
    }

    #[test]
    fn anything_else_is_an_error_with_exit_2() {
        for bad in [
            &["--verbose"][..],
            &["serve"],
            &["--version", "x"],
            &["--config", "/tmp/c.toml"],
        ] {
            let Action::Exit { code, error, text } = args(bad) else {
                panic!("{bad:?} must not run the daemon");
            };
            assert_eq!((code, error), (2, true), "{bad:?}");
            assert!(text.contains("Nothing was started"), "{text}");
        }
    }
}
