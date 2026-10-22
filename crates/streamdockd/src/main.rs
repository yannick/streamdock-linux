//! `streamdockd`: drives the StreamDock and serves the live web UI and API.

mod api;
mod assets;
mod assist;
mod audio;
mod bt_codec;
mod cli;
mod config;
mod device;
mod dsp;
mod fake;
mod hub;
mod icons;
mod mdns;
mod modes;
mod modules;
mod network;
mod output_memory;
mod player;
mod power;
mod reload;
mod render;
mod settings;
mod state;
mod version;
mod web;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use axum::{Router, routing::get};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

use crate::config::Settings;
use crate::hub::Hub;

// H15-57: return transient integration JSON allocations to the OS on small musl boards.
#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Pace of the simulated dock's input events (`fake_input = true`, demo modes only).
const FAKE_INPUT_INTERVAL: Duration = Duration::from_millis(700);

/// How long open HTTP connections (live `WatchEvents` streams never end on their own) and
/// leftover runtime work get after a shutdown signal.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

fn main() -> anyhow::Result<()> {
    // Arguments first: `--version` / `--help` / anything else must never read the config, bind a
    // port or open a dock (H15-99).
    if let cli::Action::Exit { text, code, error } = cli::parse(std::env::args().skip(1)) {
        if error {
            eprintln!("{text}");
        } else {
            println!("{text}");
        }
        std::process::exit(code);
    }
    // An explicit runtime so shutdown can't hang on a HID read parked in a blocking thread.
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let result = runtime.block_on(run());
    runtime.shutdown_timeout(SHUTDOWN_GRACE);
    result
}

async fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();
    let settings = load_settings()?;
    let addr = settings.listen;

    let hub = Hub::new();
    let (mode_switch, store, state) = start_modes(&hub, &settings)?;
    let (shutdown_tx, shutdown) = watch::channel(false);
    if settings.fake_device {
        start_fake(&hub, &settings);
    }
    let devices = settings
        .hardware
        .then(|| start_devices(&hub, &settings, state, shutdown.clone()));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!(%addr, "streamdockd listening");
    let extra = bind_extra(&settings.also_listen).await;
    // Kept until the daemon exits; dropping it unregisters the service.
    let _mdns = mdns::announce(addr);

    let stopping = shutdown.clone();
    tokio::spawn(async move {
        terminated().await;
        let _ = shutdown_tx.send(true);
    });
    let router = app(hub, mode_switch, store);
    for listener in extra {
        tokio::spawn(serve(listener, router.clone(), shutdown.clone()));
    }
    serve(listener, router, stopping).await?;
    if let Some(devices) = devices {
        let _ = devices.await;
    }
    tracing::info!("stopped");
    Ok(())
}

/// The settings from the config file (or the defaults), logged.
fn load_settings() -> anyhow::Result<Settings> {
    let settings = Settings::load()?;
    if let Some(path) = &settings.source {
        tracing::info!(config = %path.display(), "settings loaded");
    } else {
        tracing::info!("no config file; using defaults");
    }
    Ok(settings)
}

/// Starts the modules and the mode controller, with hot reload of the config. Returns the mode
/// switch, the settings store and the state file (brightness, active profile).
fn start_modes(
    hub: &Arc<Hub>,
    settings: &Settings,
) -> anyhow::Result<(modes::ModeSwitch, Arc<settings::Store>, Arc<state::StateFile>)> {
    let config_path = config::Settings::path().unwrap_or_else(|| "config.toml".into());
    hub.set_assets(Arc::new(assets::Assets::beside(&config_path)));
    let network = Arc::new(network::Network::new(settings.manage_network, settings.wifi_mode));
    if settings.manage_network {
        tokio::spawn(Arc::clone(&network).watch(Arc::clone(hub)));
        tokio::spawn(Arc::clone(&network).watch_state(Arc::clone(hub)));
    }
    hub.set_network(network);
    let context = modules::Context::new(Arc::clone(hub), &settings.modules);
    let modules = build_modules(settings, &context)?;
    let state = state_file(settings.hardware);
    start_audio(hub, settings, &state);
    let mode_switch = modes::start(
        Arc::clone(hub),
        modules,
        modes::Profiles {
            list: settings.profiles.clone(),
            state: Some(Arc::clone(&state)),
        },
    );
    // Modules built before the controller existed (the keys module's "mode" action) get it now.
    let _ = context.modes.set(mode_switch.clone());
    // Module settings apply without a restart: saved from the UI, imported, or edited on disk.
    let player = player::BuiltinPlayer::new(Arc::clone(hub), Arc::clone(&state), &config_path);
    player.apply(&settings.player);
    hub.set_builtin_player(Arc::clone(&player));
    // The wake word (H15-68): listens only when `wake_word` is set; follows reloads like the player.
    let wake = assist::wake::WakeWord::new(
        Arc::clone(hub),
        assist::wake::models::Models::beside(&config_path),
    );
    wake.apply(&settings.modules);
    hub.set_wake_word(Arc::clone(&wake));
    let reloader = Arc::new(
        reload::Reloader::new(
            Arc::clone(hub),
            mode_switch.clone(),
            (
                settings.modules.clone(),
                settings.profiles.clone(),
                settings.mode_order.clone(),
            ),
        )
        .with_player(player)
        .with_wake(wake),
    );
    tokio::spawn(Arc::clone(&reloader).watch(config_path.clone()));
    let store = Arc::new(settings::Store::new(config_path, daemon_values(settings)).with_reloader(reloader));
    Ok((mode_switch, store, state))
}

/// Attaches the simulated dock (`fake_device`).
fn start_fake(hub: &Arc<Hub>, settings: &Settings) {
    tracing::info!(
        id = fake::FAKE_ID,
        input = settings.fake_input,
        "simulating a dock (fake_device)"
    );
    let input = settings.fake_input.then_some(FAKE_INPUT_INTERVAL);
    tokio::spawn(fake::run(Arc::clone(hub), input));
}

/// Starts the device manager for the real docks (`hardware`); it ends after `shutdown`.
fn start_devices(
    hub: &Arc<Hub>,
    settings: &Settings,
    state: Arc<state::StateFile>,
    shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(device::run(
        Arc::clone(hub),
        device::Options {
            brightness: settings.brightness,
            poll_interval: settings.poll_interval,
            power: settings.power,
            state,
        },
        shutdown,
    ))
}

/// Serves `router` until `stopping` changes. Graceful shutdown waits for every connection, and a
/// browser's event stream stays open: clients get `SHUTDOWN_GRACE`, then the server stops anyway.
/// Binds the `also_listen` addresses; one that fails is logged and skipped (the main one serves).
async fn bind_extra(addrs: &[std::net::SocketAddr]) -> Vec<tokio::net::TcpListener> {
    let mut listeners = Vec::new();
    for addr in addrs {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => {
                tracing::info!(%addr, "streamdockd also listening");
                listeners.push(l);
            }
            Err(e) => tracing::warn!(%addr, error = %e, "cannot listen; skipped"),
        }
    }
    listeners
}

async fn serve(
    listener: tokio::net::TcpListener,
    router: Router,
    mut stopping: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    // With the client address, so settings changes can be limited by origin (see api.rs).
    let service = router.into_make_service_with_connect_info::<std::net::SocketAddr>();
    let server = axum::serve(listener, service).with_graceful_shutdown({
        let mut stopping = stopping.clone();
        async move {
            let _ = stopping.changed().await;
        }
    });
    tokio::select! {
        result = server => result?,
        () = async {
            let _ = stopping.changed().await;
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        } => {}
    }
    Ok(())
}

/// Resolves on Ctrl-C or SIGTERM (what systemd sends on stop).
async fn terminated() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installing the SIGTERM handler");
        tokio::select! {
            () = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await;
    tracing::info!("shutting down");
}

/// The web UI, `/health`, and the Connect API (`/streamdock.v1.DeviceService/*`).
fn app(hub: Arc<Hub>, modes: modes::ModeSwitch, settings: Arc<settings::Store>) -> Router {
    web::router()
        .route("/health", get(|| async { "ok" }))
        .merge(api::key_images(Arc::clone(&hub)))
        .fallback_service(api::router(hub, modes, settings).into_axum_service())
}

/// A settings store for tests, in a scratch file that is never written unless a test does.
/// The daemon's own settings, shown read-only on the settings page.
fn daemon_values(settings: &Settings) -> Vec<(&'static str, &'static str, String)> {
    vec![
        ("listen", "Web UI address", settings.listen.to_string()),
        ("hardware", "Real docks", settings.hardware.to_string()),
        ("fake_device", "Simulated dock", settings.fake_device.to_string()),
        (
            "brightness",
            "Default brightness",
            format!("{} %", settings.brightness),
        ),
    ]
}

/// The enabled modules in tab order.
fn build_modules(
    settings: &Settings,
    context: &modules::Context,
) -> anyhow::Result<Vec<Box<dyn modules::Module>>> {
    let mut modules = modules::from_config(&settings.modules, context)?;
    modules::sort_modes(&mut modules, &settings.mode_order);
    if !modules.is_empty() {
        let ids: Vec<_> = modules.iter().map(|m| m.id().to_owned()).collect();
        tracing::info!(?ids, "modules enabled; knob 3 switches modes");
    }
    Ok(modules)
}

/// Runtime state (brightness per dock, active profile). Dev instances without real docks keep it
/// in memory, so they never overwrite the production daemon's file.
/// With `[audio] manage = true`: the audio manager, following PipeWire (H15-50).
fn start_audio(hub: &Arc<Hub>, settings: &Settings, state: &Arc<state::StateFile>) {
    let Some(user) = &settings.audio_user else {
        return;
    };
    match audio::Audio::new(user, Arc::clone(state)) {
        Ok(audio) => {
            let audio = Arc::new(audio);
            hub.set_audio_manager(Arc::clone(&audio));
            tokio::spawn(Arc::clone(&audio).apply_changes());
            tokio::spawn(audio.watch(Arc::clone(hub)));
            // The chosen output survives audio restarts and reconnects (H15-79).
            let memory = output_memory::OutputMemory::new(Arc::clone(hub), Arc::clone(state));
            hub.set_output_memory(Arc::clone(&memory));
            tokio::spawn(memory.watch());
            // Bluetooth codecs per device (H15-78): reapplied when a headset connects.
            let codecs = bt_codec::BtCodecs::new(Arc::clone(hub), Arc::clone(state));
            hub.set_bt_codecs(Arc::clone(&codecs));
            tokio::spawn(codecs.watch());
        }
        Err(e) => tracing::warn!(error = %format!("{e:#}"), "audio not managed"),
    }
}

fn state_file(hardware: bool) -> Arc<state::StateFile> {
    Arc::new(if hardware {
        state::StateFile::default_path().map_or_else(state::StateFile::in_memory, state::StateFile::load)
    } else {
        state::StateFile::in_memory()
    })
}

#[cfg(test)]
fn test_store() -> Arc<settings::Store> {
    let path = std::env::temp_dir().join(format!("streamdockd-test-{}.toml", std::process::id()));
    Arc::new(settings::Store::new(path, Vec::new()))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use connectrpc::client::{ClientConfig, HttpClient};
    use http_body_util::BodyExt as _;
    use streamdock_hid::{InputEvent, Key, Knob};
    use streamdock_proto::streamdock::v1::__buffa::oneof::device_event::Kind;
    use streamdock_proto::streamdock::v1::__buffa::oneof::knob_event::Action;
    use streamdock_proto::streamdock::v1::{self as pb, DeviceServiceClient};
    use tower::ServiceExt as _;

    use super::*;
    use crate::hub::Command;

    #[tokio::test]
    async fn health_is_ok() {
        let res = app(Hub::new(), modes::ModeSwitch::disabled(), test_store())
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.into_body().collect().await.unwrap().to_bytes(), "ok");
    }

    #[tokio::test]
    async fn key_images_are_served_over_http() {
        let hub = Hub::new();
        let mut dock = hub.attach(fake::device());
        let task = tokio::spawn(async move {
            let req = dock.commands.recv().await.unwrap();
            req.reply.send(Ok(())).unwrap();
            dock
        });
        let png = render::render_png(&modules::face(1, "Hi", "", modules::FaceState::FACE_STATE_ON));
        hub.command(
            fake::FAKE_ID,
            hub::Command::SetKeyImage {
                key: 1,
                image: png.clone(),
            },
        )
        .await
        .unwrap();
        let app = app(Arc::clone(&hub), modes::ModeSwitch::disabled(), test_store());
        let url = format!("/dock/{}/keys/1?v=1", fake::FAKE_ID);
        let res = app
            .clone()
            .oneshot(Request::get(&url).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()["content-type"], "image/png");
        let etag = res.headers()["etag"].clone();
        assert_eq!(res.into_body().collect().await.unwrap().to_bytes(), png);
        let again = app
            .clone()
            .oneshot(
                Request::get(&url)
                    .header("if-none-match", etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
        let blank = format!("/dock/{}/keys/2", fake::FAKE_ID);
        let res = app
            .oneshot(Request::get(&blank).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        drop(task.await.unwrap());
    }

    #[tokio::test]
    async fn index_serves_html() {
        let res = app(Hub::new(), modes::ModeSwitch::disabled(), test_store())
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    type Events = connectrpc::client::ServerStream<
        <HttpClient as connectrpc::client::ClientTransport>::ResponseBody,
        pb::__buffa::view::WatchEventsResponseView<'static>,
    >;

    /// The next response that carries a device event (skips screen updates).
    async fn next_device(stream: &mut Events) -> pb::WatchEventsResponse {
        loop {
            let res = stream
                .message()
                .await
                .unwrap()
                .expect("stream ended")
                .to_owned_message();
            if res.event.kind.is_some() {
                return res;
            }
        }
    }

    /// Serves `app(hub)` on an ephemeral port and returns a Connect client for it.
    async fn serve(hub: Arc<Hub>) -> DeviceServiceClient<HttpClient> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app(hub, modes::ModeSwitch::disabled(), test_store()))
                .await
                .unwrap();
        });
        let config = ClientConfig::new(format!("http://{addr}").parse().unwrap())
            .with_default_timeout(Duration::from_secs(5));
        DeviceServiceClient::new(HttpClient::plaintext(), config)
    }

    /// The H15-6 acceptance test: a client receives the fake device's events over `WatchEvents`
    /// in binary protobuf (Connect protocol), starting with the attach snapshot.
    #[tokio::test]
    async fn watch_events_streams_fake_device_events_in_binary_protobuf() {
        let hub = Hub::new();
        let dock = hub.attach(fake::device());
        let client = serve(Arc::clone(&hub)).await;

        let mut stream = client
            .watch_events(pb::WatchEventsRequest::default())
            .await
            .unwrap();
        assert_eq!(
            stream.headers().get("content-type").unwrap(),
            "application/connect+proto",
            "binary protobuf over the Connect protocol"
        );
        let mut next = async || -> pb::WatchEventsResponse {
            stream
                .message()
                .await
                .unwrap()
                .expect("stream ended")
                .to_owned_message()
        };

        let first = next().await;
        assert!(matches!(&first.event.kind, Some(Kind::Attached(d)) if d.id == fake::FAKE_ID));
        // Then the activity history (the attach) and the screen, before any live event.
        let history = next().await;
        assert_eq!(history.activity.text, format!("dock {} attached", fake::FAKE_ID));
        let screen = next().await;
        assert!(screen.event.kind.is_none(), "expected the screen, got {screen:?}");
        assert_eq!(screen.screen.active_mode, "");
        let config = next().await;
        assert!(
            config.config.revision > 0,
            "expected the config revisions, got {config:?}"
        );

        let inputs = fake::input_cycle();
        for input in &inputs {
            dock.publish_input(*input);
        }
        let mut received = Vec::new();
        for _ in &inputs {
            let res = next().await;
            assert_eq!(res.skipped, 0);
            assert_eq!(res.event.device_id, fake::FAKE_ID);
            received.push(res.event.kind.clone().unwrap());
        }
        let expected: Vec<_> = inputs.into_iter().map(hub::input_kind).collect();
        assert_eq!(received, expected);

        drop(dock);
        assert!(matches!(&next().await.event.kind, Some(Kind::Detached(_))));
    }

    /// The UI path of H15-16: the stream carries the modes and faces, and `SetMode` switches.
    #[tokio::test]
    async fn screen_streams_modes_and_set_mode_switches() {
        let hub = Hub::new();
        let mut table = toml::Table::new();
        table.insert("pages".into(), 2.into());
        let switch = modes::start(
            Arc::clone(&hub),
            modules::demo::from_table(&table).unwrap(),
            modes::Profiles::default(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app(hub, switch, test_store()))
                .await
                .unwrap();
        });
        let config = ClientConfig::new(format!("http://{addr}").parse().unwrap())
            .with_default_timeout(Duration::from_secs(5));
        let client = DeviceServiceClient::new(HttpClient::plaintext(), config);

        let mut stream = client
            .watch_events(pb::WatchEventsRequest::default())
            .await
            .unwrap();
        let mut screen = async |want: &str| loop {
            let res = stream
                .message()
                .await
                .unwrap()
                .expect("stream ended")
                .to_owned_message();
            if res.screen.active_mode == want && !res.screen.faces.is_empty() {
                return res.screen.clone();
            }
        };
        let first = screen("demo-1").await;
        assert_eq!(first.modes.len(), 2);
        assert_eq!(first.faces.len(), 6);
        assert_eq!(
            first.knobs.iter().map(|k| k.label.as_str()).collect::<Vec<_>>(),
            ["Count", "Reset"]
        );

        client
            .set_mode(pb::SetModeRequest {
                mode: "demo-2".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        screen("demo-2").await;
        let err = client
            .set_mode(pb::SetModeRequest {
                mode: "nope".into(),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::NotFound);
    }

    /// Settings over the API: anyone may read and change them (trusted LAN, H15-22 decision); the
    /// activity feed names the address a change came from.
    #[tokio::test]
    async fn settings_read_and_change_over_the_api() {
        let path = std::env::temp_dir().join(format!("streamdockd-api-settings-{}.toml", std::process::id()));
        std::fs::write(
            &path,
            "[modules.lyrion]\nurl = \"http://a:9000\"\nplayer = \"Old\"\n",
        )
        .unwrap();
        let store = Arc::new(settings::Store::new(
            path.clone(),
            vec![("listen", "Web UI address", "x".into())],
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let service = app(Hub::new(), modes::ModeSwitch::disabled(), Arc::clone(&store))
            .into_make_service_with_connect_info::<std::net::SocketAddr>();
        tokio::spawn(async move {
            axum::serve(listener, service).await.unwrap();
        });
        let config = ClientConfig::new(format!("http://{addr}").parse().unwrap())
            .with_default_timeout(Duration::from_secs(5));
        let client = DeviceServiceClient::new(HttpClient::plaintext(), config);

        let got = client
            .get_settings(pb::GetSettingsRequest::default())
            .await
            .unwrap()
            .into_owned();
        let lyrion = got.modules.iter().find(|m| m.id == "lyrion").unwrap();
        assert_eq!(lyrion.values.get("player").map(String::as_str), Some("Old"));

        let mut req = pb::UpdateModuleSettingsRequest {
            module: "lyrion".into(),
            enabled: true,
            ..Default::default()
        };
        req.values.insert("url".into(), "http://b:9000".into());
        req.values.insert("player".into(), "Buero Essence".into());
        let res = client.update_module_settings(req).await.unwrap().into_owned();
        assert!(res.restart_required);
        assert!(std::fs::read_to_string(&path).unwrap().contains("Buero Essence"));

        // Without connect info (no peer address) changes still work.
        let token_dir = std::env::temp_dir().join(format!("streamdockd-api-token-{}", std::process::id()));
        std::fs::write(
            &path,
            format!(
                "[modules.homeassistant]\nurl = \"https://ha\"\narea = \"x\"\ntoken_file = \"{}\"\n",
                token_dir.join("token").display()
            ),
        )
        .unwrap();
        let res = app(Hub::new(), modes::ModeSwitch::disabled(), store)
            .oneshot(
                Request::post("/streamdock.v1.DeviceService/SetSecret")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"module":"homeassistant","key":"token","value":"x"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            std::fs::read_to_string(token_dir.join("token")).unwrap().trim(),
            "x"
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir_all(&token_dir).unwrap();
    }

    #[tokio::test]
    async fn every_client_gets_every_event() {
        let hub = Hub::new();
        let client = serve(Arc::clone(&hub)).await;
        let mut a = client
            .watch_events(pb::WatchEventsRequest::default())
            .await
            .unwrap();
        let mut b = client
            .watch_events(pb::WatchEventsRequest::default())
            .await
            .unwrap();

        let dock = hub.attach(fake::device());
        dock.publish_input(InputEvent::KnobTurn {
            knob: Knob::new(3).unwrap(),
            delta: 1,
        });
        for stream in [&mut a, &mut b] {
            let attached = next_device(stream).await;
            assert!(matches!(&attached.event.kind, Some(Kind::Attached(_))));
            let turn = next_device(stream).await;
            assert!(matches!(
                &turn.event.kind,
                Some(Kind::Knob(k)) if k.knob == 3 && k.action == Some(Action::Turned(1))
            ));
        }
    }

    /// A client that stops reading must not block the device or other clients; when it reads
    /// again it is told how many events it missed.
    #[tokio::test]
    async fn slow_client_skips_instead_of_blocking() {
        let hub = Hub::new();
        let dock = hub.attach(fake::device());
        let client = serve(Arc::clone(&hub)).await;
        let mut slow = client
            .watch_events(pb::WatchEventsRequest::default())
            .await
            .unwrap();
        let _attached = next_device(&mut slow).await;
        let _screen = slow.message().await.unwrap().unwrap();

        // Far more events than the buffer holds, published without any reader keeping up.
        let total = hub::EVENT_BUFFER * 4;
        for i in 0..total {
            let key = Key::new(u8::try_from(i % 9).unwrap() + 1).unwrap();
            dock.publish_input(InputEvent::KeyDown(key));
        }
        let mut fast = client
            .watch_events(pb::WatchEventsRequest::default())
            .await
            .unwrap();
        dock.publish_input(InputEvent::KeyUp(Key::new(1).unwrap()));
        let snapshot = next_device(&mut fast).await;
        assert!(matches!(&snapshot.event.kind, Some(Kind::Attached(_))));
        let live = next_device(&mut fast).await;
        assert!(matches!(&live.event.kind, Some(Kind::Key(k)) if !k.pressed));

        let mut delivered = 0_u64;
        let mut skipped = 0_u64;
        while delivered + skipped < u64::try_from(total).unwrap() + 1 {
            let res = slow.message().await.unwrap().unwrap().to_owned_message();
            delivered += 1;
            skipped += res.skipped;
        }
        assert!(skipped > 0, "the slow client should have skipped events");
    }

    #[tokio::test]
    async fn commands_reach_the_device_and_errors_map_to_connect_codes() {
        let hub = Hub::new();
        let mut dock = hub.attach(fake::device());
        let device = tokio::spawn(async move {
            let mut seen = Vec::new();
            while let Some(req) = dock.commands.recv().await {
                seen.push(req.command.clone());
                let _ = req.reply.send(Ok(()));
                if seen.len() == 3 {
                    break;
                }
            }
            (dock, seen)
        });
        let client = serve(Arc::clone(&hub)).await;

        let id = fake::FAKE_ID.to_owned();
        client
            .set_brightness(pb::SetBrightnessRequest {
                device_id: id.clone(),
                percent: 250,
                ..Default::default()
            })
            .await
            .unwrap();
        client
            .set_key_image(pb::SetKeyImageRequest {
                device_id: id.clone(),
                key: 6,
                image: vec![1, 2, 3],
                ..Default::default()
            })
            .await
            .unwrap();
        client
            .clear_key_image(pb::ClearKeyImageRequest {
                device_id: id.clone(),
                key: 2,
                ..Default::default()
            })
            .await
            .unwrap();
        let (_dock, seen) = device.await.unwrap();
        assert_eq!(
            seen,
            [
                Command::SetBrightness(100),
                Command::SetKeyImage {
                    key: 6,
                    image: vec![1, 2, 3]
                },
                Command::ClearKeyImage(2),
            ]
        );
        assert_eq!(hub.devices()[0].brightness, 100);

        let no_display = client
            .clear_key_image(pb::ClearKeyImageRequest {
                device_id: id.clone(),
                key: 7,
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(no_display.code, connectrpc::ErrorCode::InvalidArgument);
        let unknown = client
            .set_brightness(pb::SetBrightnessRequest {
                device_id: "nope".into(),
                percent: 1,
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(unknown.code, connectrpc::ErrorCode::NotFound);
    }
}
