//! Web UI: a proa-rendered shell with React islands, served from assets embedded at build time.
//!
//! `web/` builds the islands and a self-hosted React runtime into `web/dist/static/` (see
//! `build.rs`). The shell renders each island's server-side fallback, an import map that points
//! React at the embedded vendor modules, and proa's island bootstrap, which mounts the islands.

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Path, Query};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use include_dir::{Dir, include_dir};
use proa_core::{Ctx, WebRenderSync as _, WriteError};
use proa_macros::html_sync;
use proa_react::{
    HydrationStrategy, ReactBootstrapConfig, ReactModuleDescriptor, react_bootstrap, react_island,
    render_import_map_entries,
};

/// `web/dist`, embedded; its `static/` directory is served under `/static`.
static DIST: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../web/dist");

/// Import map: bare React specifiers to the embedded vendor modules (no CDN).
const IMPORT_MAP: [(&str, &str); 3] = [
    ("react", "/static/vendor/react.js"),
    ("react/jsx-runtime", "/static/vendor/react-jsx-runtime.js"),
    ("react-dom/client", "/static/vendor/react-dom-client.js"),
];

/// Islands are mounted client-side (`createRoot`) rather than hydrated: the server renders a
/// static fallback, the island then takes over with live data.
const BOOTSTRAP: ReactBootstrapConfig<'static> = ReactBootstrapConfig {
    selector: "[data-island]",
    react_import: "react",
    react_dom_client_import: "react-dom/client",
    hydrate_fn: "createRoot",
    auto_run: true,
    expose_global: None,
    use_external_runtime: true,
    react_module_url: "/static/vendor/react.js",
    react_dom_module_url: "/static/vendor/react-dom-client.js",
};

const ISLANDS: [ReactModuleDescriptor<'static>; 6] = [
    ReactModuleDescriptor {
        island: "BuildVersion",
        module_path: "/static/islands/BuildVersion.js",
        export: None,
        strategy: HydrationStrategy::Immediate,
    },
    ReactModuleDescriptor {
        island: "Changelog",
        module_path: "/static/islands/Changelog.js",
        export: None,
        strategy: HydrationStrategy::Immediate,
    },
    ReactModuleDescriptor {
        island: "Dashboard",
        module_path: "/static/islands/Dashboard.js",
        export: None,
        strategy: HydrationStrategy::Immediate,
    },
    // Settings screen (H15-22), at /settings.
    ReactModuleDescriptor {
        island: "Settings",
        module_path: "/static/islands/Settings.js",
        export: None,
        strategy: HydrationStrategy::Immediate,
    },
    // Custom keys editor (H15-29), at /keys.
    ReactModuleDescriptor {
        island: "KeysEditor",
        module_path: "/static/islands/KeysEditor.js",
        export: None,
        strategy: HydrationStrategy::Immediate,
    },
    // Living style guide of the design system (H15-11), at /design.
    ReactModuleDescriptor {
        island: "DesignSystem",
        module_path: "/static/islands/DesignSystem.js",
        export: None,
        strategy: HydrationStrategy::Immediate,
    },
];

pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/settings", get(settings))
        .route("/keys", get(keys))
        .route("/design", get(design))
        .route("/design/face.png", get(design_face))
        .route("/changelog", get(changelog))
        .route("/static/{*path}", get(static_asset))
}

/// A key face rendered exactly as the dock gets it, for the design page (H15-95):
/// `?label=Volume&level=40` (a level key; `&style=temperature|strip`, H15-97),
/// `?label=…&detail=…&state=active&icon=play`, or one key of the
/// now-playing band `?band=<text>&index=0&count=3&progress=400`.
async fn design_face(Query(q): Query<HashMap<String, String>>) -> Response {
    use streamdock_proto::streamdock::v1 as pb;
    let num = |key: &str| q.get(key).and_then(|v| v.parse::<u32>().ok());
    let text = |key: &str| q.get(key).cloned().unwrap_or_default();
    let png = if let Some(band) = q.get("band") {
        crate::render::render_band_png(&pb::BandSlice {
            text: band.clone(),
            index: num("index").unwrap_or(0),
            count: num("count").unwrap_or(1),
            progress: num("progress").unwrap_or(0),
            show_progress: num("progress").is_some(),
            ..Default::default()
        })
    } else {
        let state = match q.get("state").map(String::as_str) {
            Some("active") => pb::FaceState::FACE_STATE_ACTIVE,
            Some("on") => pb::FaceState::FACE_STATE_ON,
            _ => pb::FaceState::FACE_STATE_IDLE,
        };
        let icon = q.get("icon").filter(|i| crate::icons::exists(i));
        crate::render::render_png_with(
            &pb::KeyFace {
                label: text("label"),
                detail: text("detail"),
                level: num("level"),
                level_style: match q.get("style").map(String::as_str) {
                    Some("temperature") => pb::LevelStyle::LEVEL_STYLE_TEMPERATURE,
                    Some("strip") => pb::LevelStyle::LEVEL_STYLE_STRIP,
                    _ => pb::LevelStyle::LEVEL_STYLE_UNSPECIFIED,
                }
                .into(),
                state: state.into(),
                ..Default::default()
            },
            icon.map(|i| crate::render::KeyImage::Icon(i.as_str())),
        )
    };
    ([(header::CONTENT_TYPE, "image/png")], png).into_response()
}

/// Serves `web/dist/static/<path>` from the binary. (proa's own `EmbeddedStaticService` lives in
/// `proa_framework_axum`, which is still on axum 0.7; see ADR-001.)
async fn static_asset(Path(path): Path<String>) -> Response {
    match DIST.get_file(format!("static/{path}")) {
        Some(file) => (
            [
                (header::CONTENT_TYPE, content_type(&path)),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            file.contents(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

async fn index() -> Html<String> {
    Html(render_index().expect("rendering into a String cannot fail"))
}

fn render_index() -> Result<String, WriteError> {
    let title = "CONNECTING…";
    let props = format!(r#"{{"title":"{title}"}}"#);

    let mut cx = Ctx::new();
    html_sync! {
        <header class="hud">
            <div class="brand">
                <h1>"streamdockd"</h1>
                {build_version()}
            </div>
            <span class="meta">"StreamDock N3 control daemon"</span>
            <nav>
                <a href="/" aria-current="page">"Dashboard"</a>
                <a href="/changelog">"Changelog"</a>
                <a href="/keys">"Keys"</a>
                <a href="/settings">"Settings"</a>
                <a href="/design">"Design"</a>
            </nav>
        </header>
        <main>
            {react_island("Dashboard", "dashboard", props, |cx| {
                html_sync! {
                    <section class="panel">
                        <h2>{title}</h2>
                        <p>"Loading the live dashboard. It needs JavaScript."</p>
                    </section>
                }
                .render(cx)
            })}
        </main>
    }
    .render(&mut cx)?;

    render_runtime(&mut cx)?;

    Ok(document("streamdockd", &cx.into_output().into_string()))
}

async fn settings() -> Html<String> {
    Html(render_settings().expect("rendering into a String cannot fail"))
}

/// The settings screen (H15-22): module settings, write-only secrets.
fn render_settings() -> Result<String, WriteError> {
    let mut cx = Ctx::new();
    html_sync! {
        <header class="hud">
            <div class="brand">
                <h1>"streamdockd"</h1>
                {build_version()}
            </div>
            <span class="meta">"Settings"</span>
            <nav>
                <a href="/">"Dashboard"</a>
                <a href="/changelog">"Changelog"</a>
                <a href="/keys">"Keys"</a>
                <a href="/settings" aria-current="page">"Settings"</a>
                <a href="/design">"Design"</a>
            </nav>
        </header>
        <main>
            {react_island("Settings", "settings", "{}".to_owned(), |cx| {
                html_sync! {
                    <section class="panel">
                        <h2>"SETTINGS"</h2>
                        <p>"The settings screen needs JavaScript."</p>
                    </section>
                }
                .render(cx)
            })}
        </main>
    }
    .render(&mut cx)?;
    render_runtime(&mut cx)?;
    Ok(document(
        "streamdockd · settings",
        &cx.into_output().into_string(),
    ))
}

async fn keys() -> Html<String> {
    Html(render_keys().expect("rendering into a String cannot fail"))
}

/// The custom keys editor (H15-29): pages of keys and knobs with their actions.
fn render_keys() -> Result<String, WriteError> {
    let mut cx = Ctx::new();
    html_sync! {
        <header class="hud">
            <div class="brand">
                <h1>"streamdockd"</h1>
                {build_version()}
            </div>
            <span class="meta">"Custom keys"</span>
            <nav>
                <a href="/">"Dashboard"</a>
                <a href="/changelog">"Changelog"</a>
                <a href="/keys" aria-current="page">"Keys"</a>
                <a href="/settings">"Settings"</a>
                <a href="/design">"Design"</a>
            </nav>
        </header>
        <main>
            {react_island("KeysEditor", "keys-editor", "{}".to_owned(), |cx| {
                html_sync! {
                    <section class="panel">
                        <h2>"CUSTOM KEYS"</h2>
                        <p>"The keys editor needs JavaScript."</p>
                    </section>
                }
                .render(cx)
            })}
        </main>
    }
    .render(&mut cx)?;
    render_runtime(&mut cx)?;
    Ok(document("streamdockd · keys", &cx.into_output().into_string()))
}

async fn design() -> Html<String> {
    Html(render_design().expect("rendering into a String cannot fail"))
}

/// The design system's living style guide (H15-11): every token and component.
fn render_design() -> Result<String, WriteError> {
    let mut cx = Ctx::new();
    html_sync! {
        <header class="hud">
            <div class="brand">
                <h1>"streamdockd"</h1>
                {build_version()}
            </div>
            <span class="meta">"Design system"</span>
            <nav>
                <a href="/">"Dashboard"</a>
                <a href="/changelog">"Changelog"</a>
                <a href="/keys">"Keys"</a>
                <a href="/settings">"Settings"</a>
                <a href="/design" aria-current="page">"Design"</a>
            </nav>
        </header>
        <main>
            {react_island("DesignSystem", "design-system", "{}".to_owned(), |cx| {
                html_sync! {
                    <section class="panel">
                        <h2>"DESIGN SYSTEM"</h2>
                        <p>"The style guide needs JavaScript."</p>
                    </section>
                }
                .render(cx)
            })}
        </main>
    }
    .render(&mut cx)?;
    render_runtime(&mut cx)?;
    Ok(document("streamdockd · design", &cx.into_output().into_string()))
}

fn build_version() -> impl proa_core::WebRenderSync {
    let version = crate::version::get();
    let props = serde_json::json!({
        "revision": version.revision,
        "commit": version.commit,
        "commitUrl": version.commit_url,
        "builtAt": version.built_at.to_string(),
    })
    .to_string();
    react_island("BuildVersion", "build-version", props, move |cx| {
        html_sync! { <span class="build-version">{version.revision.clone()}</span> }.render(cx)
    })
}

async fn changelog() -> Html<String> {
    Html(render_changelog().expect("rendering into a String cannot fail"))
}

fn render_changelog() -> Result<String, WriteError> {
    let mut cx = Ctx::new();
    html_sync! {
        <header class="hud">
            <div class="brand"><h1>"streamdockd"</h1>{build_version()}</div>
            <span class="meta">"Changelog"</span>
            <nav>
                <a href="/">"Dashboard"</a>
                <a href="/changelog" aria-current="page">"Changelog"</a>
                <a href="/keys">"Keys"</a>
                <a href="/settings">"Settings"</a>
                <a href="/design">"Design"</a>
            </nav>
        </header>
        <main>
            {react_island("Changelog", "changelog", "{}".to_owned(), |cx| {
                html_sync! {
                    <section class="panel">
                        <h2>"WHAT’S NEW"</h2>
                        <pre>{include_str!("../../../CHANGELOG.md")}</pre>
                    </section>
                }.render(cx)
            })}
        </main>
    }
    .render(&mut cx)?;
    render_runtime(&mut cx)?;
    Ok(document(
        "streamdockd · changelog",
        &cx.into_output().into_string(),
    ))
}

/// Import map plus proa's island bootstrap, appended to every page body.
fn render_runtime(cx: &mut Ctx) -> Result<(), WriteError> {
    let imports: HashMap<String, String> = IMPORT_MAP
        .iter()
        .map(|&(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    render_import_map_entries(cx, &imports)?;
    react_bootstrap(&ISLANDS, &BOOTSTRAP).render(cx)
}

fn document(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"theme-color\" content=\"#02070a\">\
         <title>{title}</title><link rel=\"stylesheet\" href=\"/static/theme.css\">\
         <link rel=\"stylesheet\" href=\"/static/dashboard.css\">\
         </head><body>{body}</body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::{DIST, render_design, render_index};

    #[test]
    fn index_has_island_import_map_and_bootstrap() {
        let html = render_index().unwrap();
        assert!(html.contains(r#"data-island="Dashboard""#), "{html}");
        assert!(html.contains(r#"type="importmap""#), "{html}");
        assert!(html.contains("/static/vendor/react-dom-client.js"), "{html}");
        assert!(html.contains("/static/islands/Dashboard.js"), "{html}");
    }

    #[test]
    fn header_identifies_the_running_build_and_links_changelog() {
        let html = render_index().unwrap();
        assert!(html.contains("BuildVersion"), "{html}");
        assert!(html.contains("href=\"/changelog\""), "{html}");
    }

    #[test]
    fn design_page_mounts_the_style_guide() {
        let html = render_design().unwrap();
        assert!(html.contains(r#"data-island="DesignSystem""#), "{html}");
        assert!(html.contains("/static/islands/DesignSystem.js"), "{html}");
    }

    #[test]
    fn embedded_assets_cover_import_map_and_islands() {
        for path in [
            "static/theme.css",
            "static/vendor/react.js",
            "static/vendor/react-jsx-runtime.js",
            "static/vendor/react-dom-client.js",
            "static/islands/Dashboard.js",
            "static/dashboard.css",
            "static/islands/DesignSystem.js",
            "static/design/tokens.css",
            "static/design/components.css",
            "static/design/fonts/fira-sans-condensed-regular.woff2",
        ] {
            assert!(DIST.get_file(path).is_some(), "missing embedded asset {path}");
        }
    }
}
