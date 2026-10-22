//! `streamdock.v1.DeviceService` over connectrpc, backed by the [`Hub`].

use std::collections::VecDeque;
use std::sync::Arc;

use connectrpc::{ConnectError, RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream};
use futures_util::stream;
use streamdock_hid::{InputEvent, Key, Knob};
use streamdock_proto::streamdock::v1::__buffa::oneof::device_event::Kind;
use streamdock_proto::streamdock::v1::__buffa::oneof::knob_event::Action;
use streamdock_proto::streamdock::v1::__buffa::oneof::send_input_request::Input;
use streamdock_proto::streamdock::v1::{self as pb, DeviceService};
use tokio::sync::broadcast::error::RecvError;

use crate::hub::{Command, CommandError, Hub, HubEvent};
use crate::modes::{ModeError, ModeSwitch};
use crate::settings::{SettingsError, Store};

/// Key numbers that have a display.
const LCD_KEYS: std::ops::RangeInclusive<u32> = 1..=6;

/// The connectrpc router serving [`DeviceApi`].
pub fn router(hub: Arc<Hub>, modes: ModeSwitch, settings: Arc<Store>) -> connectrpc::Router {
    connectrpc::Router::new().add_service(Arc::new(DeviceApi { hub, modes, settings }))
}

#[derive(Debug)]
pub struct DeviceApi {
    hub: Arc<Hub>,
    modes: ModeSwitch,
    settings: Arc<Store>,
}

/// Where a request came from, for the activity feed. Anyone on the LAN may change settings
/// (decided on H15-22: trusted LAN), so the feed at least says who did.
fn peer(ctx: &RequestContext) -> String {
    ctx.extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map_or_else(|| "unknown address".to_owned(), |c| c.0.ip().to_string())
}

fn mode_error(e: &ModeError) -> ConnectError {
    match e {
        ModeError::Unknown(_) | ModeError::UnknownProfile(_) => ConnectError::not_found(e.to_string()),
        ModeError::Stopped => ConnectError::unavailable(e.to_string()),
    }
}

/// The codecs PipeWire offers for a connected device, the active one and the remembered choice.
async fn add_codecs(
    device: &mut pb::BluetoothDeviceDetail,
    codecs: &crate::bt_codec::BtCodecs,
    address: &str,
) {
    if let Some(p) = codecs.preference(address) {
        device.preferred = buffa::MessageField::some(pb::BluetoothCodecChoice {
            codec: p.codec,
            ldac_quality: p.ldac_quality.unwrap_or_default(),
            ..Default::default()
        });
    }
    device.any_link_rate = codecs.any_rate(address);
    match codecs.get(address).await {
        Ok(Some(now)) => {
            device.codecs = now
                .codecs
                .into_iter()
                .map(|c| pb::BluetoothCodec {
                    id: c.id,
                    label: c.label,
                    ..Default::default()
                })
                .collect();
            device.active_codec = now.active.unwrap_or_default();
            device.ldac_quality = now.ldac_quality.unwrap_or_default();
            device.link_rate = now.link_rate.unwrap_or_default();
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %format!("{e:#}"), "reading the Bluetooth codecs failed"),
    }
}

fn model_error(e: crate::assist::wake::models::ModelError) -> ConnectError {
    use crate::assist::wake::models::ModelError;
    match e {
        ModelError::Invalid(m) => ConnectError::invalid_argument(m),
        ModelError::NotFound(_) => ConnectError::not_found(e.to_string()),
        ModelError::Io(m) => ConnectError::unavailable(m),
    }
}

fn wake_model(m: &crate::assist::wake::models::ModelInfo) -> pb::WakeWordModel {
    pb::WakeWordModel {
        id: m.id.clone(),
        wake_word: m.wake_word.clone(),
        languages: m.languages.clone(),
        threshold: m.cutoff,
        window: u32::try_from(m.window).unwrap_or(u32::MAX),
        builtin: m.builtin,
        size_bytes: u32::try_from(m.size).unwrap_or(u32::MAX),
        author: m.author.clone(),
        ..Default::default()
    }
}

fn settings_error(e: SettingsError) -> ConnectError {
    match e {
        SettingsError::Invalid(m) => ConnectError::invalid_argument(m),
        SettingsError::NotFound(m) => ConnectError::not_found(m),
        SettingsError::Io(m) => ConnectError::unavailable(m),
    }
}

fn key_asset(assets: &crate::assets::Assets, asset: &crate::assets::Asset) -> pb::KeyAsset {
    pb::KeyAsset {
        name: asset.name.clone(),
        width: asset.width,
        height: asset.height,
        size_bytes: asset.bytes,
        preview: assets.preview(&asset.name).unwrap_or_default(),
        ..Default::default()
    }
}

/// A Wi-Fi or Bluetooth failure: the daemon's explanation (NetworkManager's or BlueZ's message).
#[allow(clippy::needless_pass_by_value)] // used as `map_err(network_error)`
fn network_error(e: anyhow::Error) -> ConnectError {
    ConnectError::unavailable(format!("{e:#}"))
}

fn bluetooth_address(address: &str) -> Result<&str, ConnectError> {
    crate::network::bluetooth::check_address(address)
        .map_err(|e| ConnectError::invalid_argument(e.to_string()))?;
    Ok(address)
}

/// The built-in icons (H15-48) as key assets, with previews when `previews`.
fn builtin_icons(previews: bool) -> Vec<pb::KeyAsset> {
    crate::icons::names()
        .map(|(name, group)| pb::KeyAsset {
            name: name.to_owned(),
            width: crate::icons::RASTER,
            height: crate::icons::RASTER,
            builtin: true,
            group: group.to_owned(),
            preview: if previews {
                crate::icons::preview(name).unwrap_or_default()
            } else {
                Vec::new()
            },
            ..Default::default()
        })
        .collect()
}

fn form(values: &buffa::MapView<'_, &str, &str>) -> std::collections::HashMap<String, String> {
    values
        .into_iter()
        .map(|&(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

// Concrete return types instead of the trait's `impl Encodable`: intended.
#[allow(refining_impl_trait)]
impl DeviceService for DeviceApi {
    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn get_version(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::GetVersionRequest>,
    ) -> ServiceResult<pb::GetVersionResponse> {
        Response::ok(crate::version::get())
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn get_devices(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::GetDevicesRequest>,
    ) -> ServiceResult<pb::GetDevicesResponse> {
        Response::ok(pb::GetDevicesResponse {
            devices: self.hub.devices(),
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn watch_events(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::WatchEventsRequest>,
    ) -> ServiceResult<ServiceStream<pb::WatchEventsResponse>> {
        let sub = self.hub.subscribe();
        let mut snapshot: VecDeque<_> = sub
            .devices
            .into_iter()
            .map(|device| {
                HubEvent::Device(pb::DeviceEvent {
                    device_id: device.id.clone(),
                    kind: Some(Kind::Attached(Box::new(device))),
                    ..Default::default()
                })
            })
            .collect();
        snapshot.extend(sub.key_images.into_iter().map(|(device_id, image)| {
            HubEvent::Device(pb::DeviceEvent {
                device_id,
                kind: Some(Kind::KeyImage(Box::new(image))),
                ..Default::default()
            })
        }));
        snapshot.extend(sub.activity.into_iter().map(HubEvent::Activity));
        snapshot.push_back(HubEvent::Screen(sub.screen));
        snapshot.extend(sub.audio.map(HubEvent::Audio));
        snapshot.extend(sub.network.map(|n| HubEvent::Network(Box::new(n))));
        snapshot.push_back(HubEvent::Config(sub.config));
        snapshot.extend(sub.player.map(HubEvent::Player));
        snapshot.extend(sub.wake.map(HubEvent::Wake));
        let rx = sub.events;
        let events = stream::unfold((snapshot, rx), |(mut snapshot, mut rx)| async move {
            if let Some(event) = snapshot.pop_front() {
                return Some((Ok(response(event, 0)), (snapshot, rx)));
            }
            let mut skipped = 0;
            loop {
                match rx.recv().await {
                    Ok(event) => return Some((Ok(response(event, skipped)), (snapshot, rx))),
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "WatchEvents client fell behind");
                        skipped += n;
                    }
                    Err(RecvError::Closed) => return None,
                }
            }
        });
        Response::ok(Box::pin(events) as ServiceStream<_>)
    }

    async fn set_brightness(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetBrightnessRequest>,
    ) -> ServiceResult<pb::SetBrightnessResponse> {
        let percent = u8::try_from(request.percent.min(100)).expect("clamped to 100");
        self.command(request.device_id, Command::SetBrightness(percent))
            .await?;
        Response::ok(pb::SetBrightnessResponse::default())
    }

    async fn set_key_image(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetKeyImageRequest>,
    ) -> ServiceResult<pb::SetKeyImageResponse> {
        let key = lcd_key(request.key)?;
        if request.image.is_empty() {
            return Err(ConnectError::invalid_argument("image is empty"));
        }
        let command = Command::SetKeyImage {
            key,
            image: request.image.to_vec(),
        };
        self.command(request.device_id, command).await?;
        Response::ok(pb::SetKeyImageResponse::default())
    }

    async fn set_mode(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetModeRequest>,
    ) -> ServiceResult<pb::SetModeResponse> {
        self.modes.set(request.mode).await.map_err(|e| mode_error(&e))?;
        Response::ok(pb::SetModeResponse::default())
    }

    async fn set_profile(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetProfileRequest>,
    ) -> ServiceResult<pb::SetProfileResponse> {
        self.modes
            .set_profile(request.profile)
            .await
            .map_err(|e| mode_error(&e))?;
        Response::ok(pb::SetProfileResponse::default())
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn send_input(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::SendInputRequest>,
    ) -> ServiceResult<pb::SendInputResponse> {
        let request = buffa::MessageView::to_owned_message(&*request)
            .map_err(|e| ConnectError::invalid_argument(e.to_string()))?;
        let input = input_event(&request)?;
        tracing::debug!(device = %request.device_id, ?input, "input from the API");
        self.hub
            .send_input(&request.device_id, input)
            .map_err(|e| ConnectError::not_found(e.to_string()))?;
        Response::ok(pb::SendInputResponse::default())
    }

    async fn get_voice(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::GetVoiceRequest>,
    ) -> ServiceResult<pb::GetVoiceResponse> {
        let voice = self.settings.voice().map_err(settings_error)?;
        let models = self.wake_models().list().iter().map(wake_model).collect();
        let microphones = crate::assist::wake::microphones(&self.hub)
            .await
            .into_iter()
            .map(|(name, description)| pb::Microphone {
                name,
                description,
                ..Default::default()
            })
            .collect();
        Response::ok(pb::GetVoiceResponse {
            available: voice.available,
            settings: buffa::MessageField::some(crate::settings::voice::voice_settings(&voice.config)),
            models,
            microphones,
            test_input: voice
                .config
                .test_input
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            ..Default::default()
        })
    }

    async fn save_voice(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SaveVoiceRequest>,
    ) -> ServiceResult<pb::SaveVoiceResponse> {
        let settings = match request.settings.as_option() {
            Some(s) => buffa::MessageView::to_owned_message(s)
                .map_err(|e| ConnectError::invalid_argument(e.to_string()))?,
            None => return Err(ConnectError::invalid_argument("no settings")),
        };
        if !settings.wake_word.is_empty() {
            self.wake_models()
                .load(&settings.wake_word)
                .map_err(model_error)?;
        }
        self.settings.save_voice(&settings).map_err(settings_error)?;
        self.settings.apply().await;
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            format!("voice settings saved from {}", peer(&ctx)),
        );
        Response::ok(pb::SaveVoiceResponse::default())
    }

    async fn list_assist_pipelines(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::ListAssistPipelinesRequest>,
    ) -> ServiceResult<pb::ListAssistPipelinesResponse> {
        let (url, token_file) = self
            .settings
            .homeassistant_target()
            .map_err(settings_error)?
            .ok_or_else(|| ConnectError::failed_precondition("Home Assistant is not set up"))?;
        let token = std::fs::read_to_string(&token_file).map_err(|e| {
            ConnectError::failed_precondition(format!("cannot read the Home Assistant token: {e}"))
        })?;
        let target = crate::assist::pipeline::Target {
            url,
            token: token.trim().to_owned(),
            pipeline: None,
            max_listen: std::time::Duration::from_secs(1),
        };
        let (pipelines, preferred) = crate::assist::pipeline::pipelines(&target)
            .await
            .map_err(|e| ConnectError::unavailable(format!("{e:#}")))?;
        Response::ok(pb::ListAssistPipelinesResponse {
            pipelines: pipelines
                .into_iter()
                .map(|p| pb::AssistPipeline {
                    id: p.id,
                    name: p.name,
                    language: p.language,
                    stt_language: p.stt_language,
                    ..Default::default()
                })
                .collect(),
            preferred,
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn upload_wake_word(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::UploadWakeWordRequest>,
    ) -> ServiceResult<pb::UploadWakeWordResponse> {
        let wake = self
            .hub
            .wake_word()
            .ok_or_else(|| ConnectError::unavailable("this daemon has no wake word service"))?;
        let info = wake
            .models()
            .save(request.model, request.manifest)
            .map_err(model_error)?;
        // The listener reads its model when it starts: restart it if it uses this one.
        let in_use = self.settings.voice().ok().and_then(|v| v.config.wake_word) == Some(info.id.clone());
        if in_use && let Ok(settings) = crate::config::Settings::load() {
            wake.restart(&settings.modules);
        }
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            format!("wake word \"{}\" uploaded from {}", info.wake_word, peer(&ctx)),
        );
        Response::ok(pb::UploadWakeWordResponse {
            model: buffa::MessageField::some(wake_model(&info)),
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn delete_wake_word(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DeleteWakeWordRequest>,
    ) -> ServiceResult<pb::DeleteWakeWordResponse> {
        let wake = self
            .hub
            .wake_word()
            .ok_or_else(|| ConnectError::unavailable("this daemon has no wake word service"))?;
        let in_use = self.settings.voice().ok().and_then(|v| v.config.wake_word);
        if in_use.as_deref() == Some(request.id) {
            return Err(ConnectError::failed_precondition(
                "this wake word is in use: choose another one (or none) first",
            ));
        }
        wake.models().delete(request.id).map_err(model_error)?;
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            format!("wake word {} deleted from {}", request.id, peer(&ctx)),
        );
        Response::ok(pb::DeleteWakeWordResponse::default())
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn get_output_priority(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::GetOutputPriorityRequest>,
    ) -> ServiceResult<pb::GetOutputPriorityResponse> {
        let memory = self
            .hub
            .output_memory()
            .ok_or_else(|| ConnectError::unavailable("this daemon doesn't manage the audio"))?;
        Response::ok(pb::GetOutputPriorityResponse {
            priority: buffa::MessageField::some(memory.get()),
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn set_output_priority(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetOutputPriorityRequest>,
    ) -> ServiceResult<pb::SetOutputPriorityResponse> {
        let memory = self
            .hub
            .output_memory()
            .ok_or_else(|| ConnectError::unavailable("this daemon doesn't manage the audio"))?;
        let priority = match request.priority.as_option() {
            Some(p) => buffa::MessageView::to_owned_message(p)
                .map_err(|e| ConnectError::invalid_argument(e.to_string()))?,
            None => pb::OutputPriority::default(),
        };
        memory
            .set(&priority)
            .map_err(|e| ConnectError::invalid_argument(format!("{e:#}")))?;
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            format!("output priority saved from {}", peer(&ctx)),
        );
        Response::ok(pb::SetOutputPriorityResponse::default())
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn get_settings(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::GetSettingsRequest>,
    ) -> ServiceResult<pb::GetSettingsResponse> {
        Response::ok(self.settings.get().map_err(settings_error)?)
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn update_module_settings(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::UpdateModuleSettingsRequest>,
    ) -> ServiceResult<pb::UpdateModuleSettingsResponse> {
        self.settings
            .update(request.module, request.enabled, &form(&request.values))
            .map_err(settings_error)?;
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            format!("settings of {} saved from {}", request.module, peer(&ctx)),
        );
        Response::ok(pb::UpdateModuleSettingsResponse {
            restart_required: !self.settings.apply().await,
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn set_secret(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetSecretRequest>,
    ) -> ServiceResult<pb::SetSecretResponse> {
        self.settings
            .set_secret(request.module, request.key, request.value)
            .map_err(settings_error)?;
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            format!("{} {} stored from {}", request.module, request.key, peer(&ctx)),
        );
        Response::ok(pb::SetSecretResponse::default())
    }

    async fn add_tab(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::AddTabRequest>,
    ) -> ServiceResult<pb::AddTabResponse> {
        let mode = self
            .settings
            .add_tab(
                request.kind,
                request.title,
                request.area,
                request.player,
                request.profile,
            )
            .map_err(settings_error)?;
        self.changed(&ctx, format!("tab {} added", request.title)).await;
        Response::ok(pb::AddTabResponse {
            mode,
            ..Default::default()
        })
    }

    async fn rename_tab(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RenameTabRequest>,
    ) -> ServiceResult<pb::RenameTabResponse> {
        self.settings
            .rename_tab(request.mode, request.title)
            .map_err(settings_error)?;
        self.changed(&ctx, format!("tab {} renamed to {}", request.mode, request.title))
            .await;
        Response::ok(pb::RenameTabResponse::default())
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn get_music_tab(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetMusicTabRequest>,
    ) -> ServiceResult<pb::GetMusicTabResponse> {
        let (player, effective_player) = self
            .settings
            .music_tab_player(request.mode)
            .map_err(settings_error)?;
        Response::ok(pb::GetMusicTabResponse {
            player,
            effective_player,
            ..Default::default()
        })
    }

    async fn set_music_tab_player(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetMusicTabPlayerRequest>,
    ) -> ServiceResult<pb::SetMusicTabPlayerResponse> {
        self.settings
            .set_music_tab_player(request.mode, request.player)
            .map_err(settings_error)?;
        self.changed(
            &ctx,
            format!("tab {} now controls player {}", request.mode, request.player),
        )
        .await;
        Response::ok(pb::SetMusicTabPlayerResponse::default())
    }

    async fn delete_tab(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DeleteTabRequest>,
    ) -> ServiceResult<pb::DeleteTabResponse> {
        self.settings.delete_tab(request.mode).map_err(settings_error)?;
        self.changed(&ctx, format!("tab {} deleted", request.mode)).await;
        Response::ok(pb::DeleteTabResponse::default())
    }

    async fn move_tabs(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::MoveTabsRequest>,
    ) -> ServiceResult<pb::MoveTabsResponse> {
        let order: Vec<String> = request.order.iter().map(|m| (*m).to_owned()).collect();
        self.settings
            .move_tabs(&order, request.profile)
            .map_err(settings_error)?;
        self.changed(&ctx, "tabs reordered".to_owned()).await;
        Response::ok(pb::MoveTabsResponse::default())
    }

    async fn convert_tab(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ConvertTabRequest>,
    ) -> ServiceResult<pb::ConvertTabResponse> {
        let mode = self
            .settings
            .convert_tab(request.mode)
            .await
            .map_err(settings_error)?;
        self.changed(
            &ctx,
            format!("tab {} converted to custom keys ({mode})", request.mode),
        )
        .await;
        Response::ok(pb::ConvertTabResponse {
            mode,
            ..Default::default()
        })
    }

    async fn get_lights_tab(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetLightsTabRequest>,
    ) -> ServiceResult<pb::GetLightsTabResponse> {
        let tab = self
            .settings
            .lights_tab_live(request.mode)
            .await
            .map_err(settings_error)?;
        let [knob1, knob2] = tab.knobs;
        Response::ok(pb::GetLightsTabResponse {
            area: tab.area,
            pinned: tab.pinned,
            keys: tab
                .keys
                .into_iter()
                .map(|pin| pb::LightsKey {
                    key: u32::from(pin.key),
                    entity: pin.entity,
                    label: pin.label.unwrap_or_default(),
                    action: pin.action,
                    service: pin.service,
                    icon: pin.icon,
                    ..Default::default()
                })
                .collect(),
            knob1,
            knob2,
            ..Default::default()
        })
    }

    async fn set_lights_key(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetLightsKeyRequest>,
    ) -> ServiceResult<pb::SetLightsKeyResponse> {
        let key = u8::try_from(request.key).map_err(|_| ConnectError::invalid_argument("key must be 1-6"))?;
        self.settings
            .set_lights_key(
                request.mode,
                crate::modules::homeassistant::Pin {
                    key,
                    entity: request.entity.to_owned(),
                    label: Some(request.label.to_owned()),
                    action: request.action.to_owned(),
                    service: request.service.to_owned(),
                    icon: request.icon.to_owned(),
                },
            )
            .await
            .map_err(settings_error)?;
        let what = if request.entity.is_empty() {
            "cleared".to_owned()
        } else {
            format!("= {}", request.entity)
        };
        self.changed(&ctx, format!("{} key {key} {what}", request.mode))
            .await;
        Response::ok(pb::SetLightsKeyResponse::default())
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn get_now_playing(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetNowPlayingRequest>,
    ) -> ServiceResult<pb::GetNowPlayingResponse> {
        let config = self.settings.now_playing(request.mode).map_err(settings_error)?;
        Response::ok(pb::GetNowPlayingResponse {
            settings: config.to_message().into(),
            ..Default::default()
        })
    }

    async fn set_now_playing(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetNowPlayingRequest>,
    ) -> ServiceResult<pb::SetNowPlayingResponse> {
        let settings = request
            .settings
            .as_option()
            .ok_or_else(|| ConnectError::invalid_argument("settings are missing"))?;
        let config = crate::modules::lyrion::now_playing::Config::from_message(settings)
            .map_err(|e| ConnectError::invalid_argument(format!("{e:#}")))?;
        self.settings
            .set_now_playing(request.mode, &config)
            .map_err(settings_error)?;
        self.changed(&ctx, format!("{} now playing", request.mode)).await;
        Response::ok(pb::SetNowPlayingResponse::default())
    }

    async fn set_lights_knob(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetLightsKnobRequest>,
    ) -> ServiceResult<pb::SetLightsKnobResponse> {
        self.settings
            .set_lights_knob(request.mode, request.knob, request.function)
            .map_err(settings_error)?;
        self.changed(
            &ctx,
            format!("{} knob {} = {}", request.mode, request.knob, request.function),
        )
        .await;
        Response::ok(pb::SetLightsKnobResponse::default())
    }

    async fn list_home_assistant_entities(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::ListHomeAssistantEntitiesRequest>,
    ) -> ServiceResult<pb::ListHomeAssistantEntitiesResponse> {
        let entities = self.settings.ha_entities().await.map_err(settings_error)?;
        Response::ok(pb::ListHomeAssistantEntitiesResponse {
            entities: entities
                .into_iter()
                .map(|e| pb::HomeAssistantEntity {
                    entity_id: e.entity_id,
                    name: e.name,
                    area: e.area,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
    }

    async fn get_setting_choices(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetSettingChoicesRequest>,
    ) -> ServiceResult<pb::GetSettingChoicesResponse> {
        let choices = crate::settings::choices(request.module, request.key, &form(&request.values))
            .await
            .map_err(settings_error)?;
        Response::ok(pb::GetSettingChoicesResponse {
            options: choices
                .iter()
                .map(|c| pb::SettingChoice {
                    value: c.value.clone(),
                    label: c.label.clone(),
                    detail: c.detail.clone(),
                    ..Default::default()
                })
                .collect(),
            choices: choices.into_iter().map(|c| c.value).collect(),
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn export_backup(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::ExportBackupRequest>,
    ) -> ServiceResult<pb::ExportBackupResponse> {
        let (data, file_name) = self.settings.export_backup().map_err(settings_error)?;
        Response::ok(pb::ExportBackupResponse {
            data,
            file_name,
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn import_backup(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ImportBackupRequest>,
    ) -> ServiceResult<pb::ImportBackupResponse> {
        let imported = self
            .settings
            .import_backup(request.data)
            .map_err(settings_error)?;
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            format!("configuration imported from a backup ({})", peer(&ctx)),
        );
        let applied = self.settings.apply().await;
        self.hub.assets_changed();
        Response::ok(pb::ImportBackupResponse {
            restart_required: imported.daemon_changed || !applied,
            missing_secrets: imported.missing_secrets,
            notes: imported.notes,
            ..Default::default()
        })
    }

    async fn get_network(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::GetNetworkRequest>,
    ) -> ServiceResult<pb::GetNetworkResponse> {
        match self.hub.network() {
            Some(network) => Response::ok(network.status().await),
            None => Response::ok(pb::GetNetworkResponse::default()),
        }
    }

    async fn list_wifi_networks(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListWifiNetworksRequest>,
    ) -> ServiceResult<pb::ListWifiNetworksResponse> {
        let networks = self
            .network()?
            .wifi_networks(request.rescan)
            .await
            .map_err(network_error)?;
        Response::ok(pb::ListWifiNetworksResponse {
            networks,
            ..Default::default()
        })
    }

    async fn connect_wifi(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ConnectWifiRequest>,
    ) -> ServiceResult<pb::ConnectWifiResponse> {
        let ssid = request.ssid.trim();
        if ssid.is_empty() {
            return Err(ConnectError::invalid_argument("choose a network"));
        }
        let wifi = self.network()?.wifi().await.map_err(network_error)?;
        wifi.connect(ssid, request.password)
            .await
            .map_err(network_error)?;
        self.report_network(&ctx, &format!("Wi-Fi connected to {ssid:?}"));
        Response::ok(pb::ConnectWifiResponse::default())
    }

    async fn disconnect_wifi(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::DisconnectWifiRequest>,
    ) -> ServiceResult<pb::DisconnectWifiResponse> {
        let wifi = self.network()?.wifi().await.map_err(network_error)?;
        wifi.disconnect().await.map_err(network_error)?;
        self.report_network(&ctx, "Wi-Fi disconnected");
        Response::ok(pb::DisconnectWifiResponse::default())
    }

    async fn forget_wifi(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ForgetWifiRequest>,
    ) -> ServiceResult<pb::ForgetWifiResponse> {
        let wifi = self.network()?.wifi().await.map_err(network_error)?;
        let forgotten = wifi.forget(request.ssid).await.map_err(network_error)?;
        if forgotten {
            self.report_network(&ctx, &format!("Wi-Fi network {:?} forgotten", request.ssid));
        }
        Response::ok(pb::ForgetWifiResponse {
            forgotten,
            ..Default::default()
        })
    }

    async fn list_bluetooth_devices(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListBluetoothDevicesRequest>,
    ) -> ServiceResult<pb::ListBluetoothDevicesResponse> {
        let (devices, scanning) = self
            .network()?
            .bluetooth_devices(request.scan)
            .await
            .map_err(network_error)?;
        Response::ok(pb::ListBluetoothDevicesResponse {
            devices,
            scanning,
            ..Default::default()
        })
    }

    async fn get_bluetooth_device(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetBluetoothDeviceRequest>,
    ) -> ServiceResult<pb::GetBluetoothDeviceResponse> {
        let address = bluetooth_address(request.address)?;
        let mut device = self
            .network()?
            .bluetooth_detail(address)
            .await
            .map_err(network_error)?;
        if let Some(codecs) = self.hub.bt_codecs() {
            add_codecs(&mut device, codecs, address).await;
        }
        Response::ok(pb::GetBluetoothDeviceResponse {
            device: buffa::MessageField::some(device),
            ..Default::default()
        })
    }

    async fn set_bluetooth_codec(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetBluetoothCodecRequest>,
    ) -> ServiceResult<pb::SetBluetoothCodecResponse> {
        let address = bluetooth_address(request.address)?;
        let codecs = self
            .hub
            .bt_codecs()
            .ok_or_else(|| ConnectError::unavailable("this daemon doesn't manage the audio"))?;
        let (codec, quality) = request
            .choice
            .as_option()
            .map(|c| (c.codec.to_owned(), c.ldac_quality.to_owned()))
            .unwrap_or_default();
        let result = if codec.is_empty() {
            codecs.reset(address).await
        } else {
            codecs
                .set(
                    address,
                    crate::bt_codec::Preference {
                        codec: codec.clone(),
                        ldac_quality: Some(quality).filter(|q| !q.is_empty() && q != "auto"),
                    },
                )
                .await
        };
        result.map_err(|e| ConnectError::failed_precondition(format!("{e:#}")))?;
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            format!(
                "Bluetooth {address}: codec {} from {}",
                if codec.is_empty() { "automatic" } else { &codec },
                peer(&ctx)
            ),
        );
        Response::ok(pb::SetBluetoothCodecResponse::default())
    }

    async fn set_bluetooth_link_rate(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetBluetoothLinkRateRequest>,
    ) -> ServiceResult<pb::SetBluetoothLinkRateResponse> {
        let address = bluetooth_address(request.address)?;
        let codecs = self
            .hub
            .bt_codecs()
            .ok_or_else(|| ConnectError::unavailable("this daemon doesn't manage the audio"))?;
        let any = request.any_rate;
        codecs
            .set_link_rate(address, any)
            .await
            .map_err(|e| ConnectError::failed_precondition(format!("{e:#}")))?;
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            format!(
                "Bluetooth {address}: link rate {} from {}",
                if any { "as the headset chooses" } else { "48 kHz" },
                peer(&ctx)
            ),
        );
        Response::ok(pb::SetBluetoothLinkRateResponse::default())
    }

    async fn connect_bluetooth(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ConnectBluetoothRequest>,
    ) -> ServiceResult<pb::ConnectBluetoothResponse> {
        let address = bluetooth_address(request.address)?;
        let bt = self.network()?.bluetooth().await.map_err(network_error)?;
        bt.connect(address).await.map_err(network_error)?;
        self.report_network(&ctx, &format!("Bluetooth {address} connected"));
        Response::ok(pb::ConnectBluetoothResponse::default())
    }

    async fn disconnect_bluetooth(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DisconnectBluetoothRequest>,
    ) -> ServiceResult<pb::DisconnectBluetoothResponse> {
        let address = bluetooth_address(request.address)?;
        let bt = self.network()?.bluetooth().await.map_err(network_error)?;
        bt.disconnect(address).await.map_err(network_error)?;
        self.report_network(&ctx, &format!("Bluetooth {address} disconnected"));
        Response::ok(pb::DisconnectBluetoothResponse::default())
    }

    async fn remove_bluetooth(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RemoveBluetoothRequest>,
    ) -> ServiceResult<pb::RemoveBluetoothResponse> {
        let address = bluetooth_address(request.address)?;
        let bt = self.network()?.bluetooth().await.map_err(network_error)?;
        bt.remove(address).await.map_err(network_error)?;
        self.report_network(&ctx, &format!("Bluetooth {address} removed"));
        Response::ok(pb::RemoveBluetoothResponse::default())
    }

    async fn set_bluetooth_power(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetBluetoothPowerRequest>,
    ) -> ServiceResult<pb::SetBluetoothPowerResponse> {
        let bt = self.network()?.bluetooth().await.map_err(network_error)?;
        bt.set_powered(request.on).await.map_err(network_error)?;
        self.report_network(
            &ctx,
            if request.on {
                "Bluetooth on"
            } else {
                "Bluetooth off"
            },
        );
        Response::ok(pb::SetBluetoothPowerResponse::default())
    }

    async fn set_ethernet(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetEthernetRequest>,
    ) -> ServiceResult<pb::SetEthernetResponse> {
        let ipv4 = if request.keep_ipv4 {
            None
        } else if request.dhcp {
            Some(crate::network::ethernet::Ipv4::Dhcp)
        } else {
            let dns: Vec<String> = request.dns.iter().map(|d| (*d).to_owned()).collect();
            Some(
                crate::network::ethernet::parse_static(
                    request.address,
                    request.prefix,
                    request.gateway,
                    &dns,
                )
                .map_err(|e| ConnectError::invalid_argument(format!("{e:#}")))?,
            )
        };
        let ipv6 = match request.ipv6_method {
            "" => None,
            method => Some(
                crate::network::ethernet::parse_ipv6(
                    method,
                    request.ipv6_address,
                    request.ipv6_prefix,
                    request.ipv6_gateway,
                    &request
                        .ipv6_dns
                        .iter()
                        .map(|d| (*d).to_owned())
                        .collect::<Vec<_>>(),
                )
                .map_err(|e| ConnectError::invalid_argument(format!("{e:#}")))?,
            ),
        };
        let revert = self
            .network()?
            .try_ethernet(ipv4.as_ref(), ipv6.as_ref())
            .await
            .map_err(network_error)?;
        self.report_network(
            &ctx,
            &format!("Ethernet change on trial, reverted in {revert} s unless kept"),
        );
        Response::ok(pb::SetEthernetResponse {
            revert_in_seconds: revert,
            ..Default::default()
        })
    }

    async fn set_wifi_mode(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetWifiModeRequest>,
    ) -> ServiceResult<pb::SetWifiModeResponse> {
        let mode = crate::network::WifiMode::parse(request.mode)
            .map_err(|e| ConnectError::invalid_argument(e.to_string()))?;
        let network = self.network()?;
        self.settings
            .set_network_wifi(mode.as_str())
            .map_err(settings_error)?;
        network.set_wifi_mode(mode).await.map_err(network_error)?;
        self.report_network(&ctx, &format!("Wi-Fi mode {}", mode.as_str()));
        Response::ok(pb::SetWifiModeResponse::default())
    }

    async fn get_audio(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::GetAudioRequest>,
    ) -> ServiceResult<pb::GetAudioResponse> {
        let Some(audio) = self.hub.audio() else {
            return Response::ok(pb::GetAudioResponse::default());
        };
        Response::ok(pb::GetAudioResponse {
            managed: true,
            state: audio.state().await.into(),
            ..Default::default()
        })
    }

    async fn set_audio_output(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetAudioOutputRequest>,
    ) -> ServiceResult<pb::SetAudioOutputResponse> {
        self.audio()?.select(request.name).await.map_err(network_error)?;
        self.report_network(&ctx, &format!("audio output {}", request.name));
        Response::ok(pb::SetAudioOutputResponse::default())
    }

    async fn set_audio_volume(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetAudioVolumeRequest>,
    ) -> ServiceResult<pb::SetAudioVolumeResponse> {
        // Not reported in the activity feed: a slider sends many. Applied in the background (the
        // latest wins); the new value is published right away.
        let audio = self.audio()?;
        audio
            .set_volume(request.name, request.volume_percent, request.muted)
            .await
            .map_err(network_error)?;
        self.hub.set_audio(audio.current().await);
        Response::ok(pb::SetAudioVolumeResponse::default())
    }

    async fn get_player(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::GetPlayerRequest>,
    ) -> ServiceResult<pb::GetPlayerResponse> {
        let settings = self.settings.player().map_err(settings_error)?;
        let state = self.hub.player_state().unwrap_or_else(|| pb::PlayerState {
            player_id: self
                .hub
                .builtin_player()
                .map(|p| p.player_id())
                .unwrap_or_default(),
            ..Default::default()
        });
        Response::ok(pb::GetPlayerResponse {
            settings: crate::player::settings_message(&settings).into(),
            outputs: self.player_outputs().await,
            state: state.into(),
            sample_rates: crate::config::PlayerSettings::SAMPLE_RATES.to_vec(),
            ..Default::default()
        })
    }

    async fn set_player(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetPlayerRequest>,
    ) -> ServiceResult<pb::SetPlayerResponse> {
        let s = request
            .settings
            .as_option()
            .ok_or_else(|| ConnectError::invalid_argument("settings are required"))?;
        let settings = crate::player::settings_from_message(s)
            .map_err(|e| ConnectError::invalid_argument(format!("{e:#}")))?;
        self.settings.set_player(&settings).map_err(settings_error)?;
        if !self.settings.apply().await {
            return Err(ConnectError::unavailable("saved; applies after a restart"));
        }
        self.hub.report(
            &peer(&ctx),
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            format!(
                "player settings saved ({})",
                if settings.enabled { "on" } else { "off" }
            ),
        );
        Response::ok(pb::SetPlayerResponse::default())
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn reconnect_player(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::ReconnectPlayerRequest>,
    ) -> ServiceResult<pb::ReconnectPlayerResponse> {
        let player = self.builtin_player()?;
        if !player.restart() {
            return Err(ConnectError::failed_precondition("the player is off"));
        }
        self.hub.report(
            &peer(&ctx),
            pb::ActivityLevel::ACTIVITY_LEVEL_INFO,
            "player reconnecting".to_owned(),
        );
        Response::ok(pb::ReconnectPlayerResponse::default())
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn reset_player_id(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::ResetPlayerIdRequest>,
    ) -> ServiceResult<pb::ResetPlayerIdResponse> {
        let player_id = self.builtin_player()?.reset_id();
        self.hub.report(
            &peer(&ctx),
            pb::ActivityLevel::ACTIVITY_LEVEL_INFO,
            format!("new player id {player_id}"),
        );
        Response::ok(pb::ResetPlayerIdResponse {
            player_id,
            ..Default::default()
        })
    }

    async fn find_lyrion_servers(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::FindLyrionServersRequest>,
    ) -> ServiceResult<pb::FindLyrionServersResponse> {
        let servers = slimproto::discovery::discover(std::time::Duration::from_secs(2))
            .await
            .map_err(|e| ConnectError::unavailable(format!("discovery failed: {e}")))?;
        Response::ok(pb::FindLyrionServersResponse {
            servers: servers
                .into_iter()
                .map(|s| pb::LyrionServer {
                    address: s.addr.to_string(),
                    name: s.name,
                    version: s.version,
                    http_port: s.http_port.map_or(0, u32::from),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
    }

    async fn set_audio_tone(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetAudioToneRequest>,
    ) -> ServiceResult<pb::SetAudioToneResponse> {
        let t = request
            .tone
            .as_option()
            .ok_or_else(|| ConnectError::invalid_argument("tone is required"))?;
        let dsp = crate::dsp::Dsp {
            bass_db: t.bass_db,
            mid_db: t.mid_db,
            treble_db: t.treble_db,
            balance: t.balance,
            warmth: t.warmth,
            energy: t.energy,
            loudness: t.loudness,
            correction: t.correction.to_owned(),
            bands: t
                .bands
                .iter()
                .map(|b| crate::dsp::Band {
                    freq_hz: b.freq_hz,
                    gain_db: b.gain_db,
                    q: b.q,
                })
                .collect(),
            preamp_db: t.preamp_db,
            crossfeed: t.crossfeed,
            crossfeed_db: t.crossfeed_db,
            limit_db: t.limit_db,
        };
        // Not reported in the activity feed: sliders send many. Applied in the background (the
        // latest wins); the settings live in our state file, so publish them now.
        let audio = self.audio()?;
        audio.set_dsp(dsp);
        self.hub.set_audio(audio.current().await);
        Response::ok(pb::SetAudioToneResponse::default())
    }

    async fn confirm_ethernet(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::ConfirmEthernetRequest>,
    ) -> ServiceResult<pb::ConfirmEthernetResponse> {
        self.network()?.confirm_ethernet().await.map_err(network_error)?;
        self.report_network(&ctx, "Ethernet change kept");
        Response::ok(pb::ConfirmEthernetResponse::default())
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn get_key_pages(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::GetKeyPagesRequest>,
    ) -> ServiceResult<pb::GetKeyPagesResponse> {
        let mut response = self.settings.key_pages().map_err(settings_error)?;
        if let Some(assets) = self.hub.assets() {
            response.images = assets
                .list()
                .map_err(|e| ConnectError::unavailable(format!("{e:#}")))?
                .into_iter()
                .map(|a| a.name)
                .collect();
        }
        response.icons = builtin_icons(false);
        Response::ok(response)
    }

    async fn save_key_pages(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SaveKeyPagesRequest>,
    ) -> ServiceResult<pb::SaveKeyPagesResponse> {
        let pages = match request.pages.as_option() {
            Some(p) => buffa::MessageView::to_owned_message(p)
                .map_err(|e| ConnectError::invalid_argument(e.to_string()))?,
            None => pb::KeyPages::default(),
        };
        self.settings.save_key_pages(&pages).map_err(settings_error)?;
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            format!("custom keys saved from {}", peer(&ctx)),
        );
        Response::ok(pb::SaveKeyPagesResponse {
            restart_required: !self.settings.apply().await,
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn list_key_assets(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::ListKeyAssetsRequest>,
    ) -> ServiceResult<pb::ListKeyAssetsResponse> {
        let assets = self.assets()?;
        let list = assets
            .list()
            .map_err(|e| ConnectError::unavailable(format!("{e:#}")))?;
        let uploaded: Vec<pb::KeyAsset> = list.iter().map(|a| key_asset(assets, a)).collect();
        let icons = builtin_icons(true)
            .into_iter()
            .filter(|i| !uploaded.iter().any(|u| u.name == i.name));
        Response::ok(pb::ListKeyAssetsResponse {
            assets: uploaded.iter().cloned().chain(icons).collect(),
            ..Default::default()
        })
    }

    async fn set_key_asset(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetKeyAssetRequest>,
    ) -> ServiceResult<pb::SetKeyAssetResponse> {
        let assets = self.assets()?;
        let name = request.name.to_owned();
        let data = request.data.to_vec();
        let store = Arc::clone(assets);
        let stored = tokio::task::spawn_blocking(move || store.put(&name, &data))
            .await
            .map_err(|e| ConnectError::internal(e.to_string()))?
            .map_err(|e| ConnectError::invalid_argument(format!("{e:#}")))?;
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            format!("key image {:?} stored from {}", stored.name, peer(&ctx)),
        );
        self.hub.assets_changed();
        Response::ok(pb::SetKeyAssetResponse {
            asset: buffa::MessageField::some(key_asset(assets, &stored)),
            ..Default::default()
        })
    }

    #[allow(clippy::unused_async_trait_impl)] // the trait method is async
    async fn delete_key_asset(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DeleteKeyAssetRequest>,
    ) -> ServiceResult<pb::DeleteKeyAssetResponse> {
        let deleted = self
            .assets()?
            .delete(request.name)
            .map_err(|e| ConnectError::invalid_argument(format!("{e:#}")))?;
        if !deleted && crate::icons::exists(request.name) {
            return Err(ConnectError::failed_precondition(format!(
                "{:?} is a built-in icon; it can't be deleted (upload an image of that name to replace it)",
                request.name
            )));
        }
        if deleted {
            self.hub.report(
                crate::hub::DAEMON,
                pb::ActivityLevel::ACTIVITY_LEVEL_OK,
                format!("key image {:?} deleted from {}", request.name, peer(&ctx)),
            );
        }
        self.hub.assets_changed();
        Response::ok(pb::DeleteKeyAssetResponse {
            deleted,
            ..Default::default()
        })
    }

    async fn clear_key_image(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::ClearKeyImageRequest>,
    ) -> ServiceResult<pb::ClearKeyImageResponse> {
        let key = lcd_key(request.key)?;
        self.command(request.device_id, Command::ClearKeyImage(key))
            .await?;
        Response::ok(pb::ClearKeyImageResponse::default())
    }
}

impl DeviceApi {
    /// The wake word models: the running service's, or only the built-in ones (tests, tools).
    fn wake_models(&self) -> crate::assist::wake::models::Models {
        self.hub.wake_word().map_or_else(
            || crate::assist::wake::models::Models::new(std::path::PathBuf::from("/nonexistent")),
            |w| w.models().clone(),
        )
    }

    fn builtin_player(&self) -> Result<&Arc<crate::player::BuiltinPlayer>, ConnectError> {
        self.hub
            .builtin_player()
            .ok_or_else(|| ConnectError::unavailable("no built-in player in this daemon"))
    }

    /// The outputs the player may play on, where the daemon manages the audio.
    async fn player_outputs(&self) -> Vec<pb::PlayerOutput> {
        let Some(audio) = self.hub.audio() else {
            return Vec::new();
        };
        audio
            .state()
            .await
            .outputs
            .into_iter()
            .map(|o| pb::PlayerOutput {
                name: o.name,
                description: o.description,
                ..Default::default()
            })
            .collect()
    }

    fn network(&self) -> Result<&Arc<crate::network::Network>, ConnectError> {
        match self.hub.network() {
            Some(n) if n.managed() => Ok(n),
            _ => Err(ConnectError::failed_precondition(
                "this streamdockd doesn't manage the network; set [network] manage = true",
            )),
        }
    }

    fn audio(&self) -> Result<&Arc<crate::audio::Audio>, ConnectError> {
        self.hub.audio().ok_or_else(|| {
            ConnectError::failed_precondition(
                "this streamdockd doesn't manage the audio; set [audio] manage = true",
            )
        })
    }

    /// After a network change: report it with its origin and push the new state (H15-49).
    fn report_network(&self, ctx: &RequestContext, what: &str) {
        if let Some(network) = self.hub.network() {
            network.changed();
        }
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            format!("{what} (from {})", peer(ctx)),
        );
    }

    /// After a tab change: report it with its origin and apply it (hot reload).
    async fn changed(&self, ctx: &RequestContext, what: String) {
        self.hub.report(
            crate::hub::DAEMON,
            pb::ActivityLevel::ACTIVITY_LEVEL_OK,
            format!("{what} (from {})", peer(ctx)),
        );
        self.settings.apply().await;
    }

    fn assets(&self) -> Result<&Arc<crate::assets::Assets>, ConnectError> {
        self.hub
            .assets()
            .ok_or_else(|| ConnectError::unavailable("this daemon has no key image store (no config path)"))
    }

    async fn command(&self, device_id: &str, command: Command) -> Result<(), ConnectError> {
        self.hub.command(device_id, command).await.map_err(|e| match e {
            CommandError::NotFound(_) => ConnectError::not_found(e.to_string()),
            CommandError::Gone(_) => ConnectError::unavailable(e.to_string()),
            CommandError::Failed { .. } => ConnectError::internal(e.to_string()),
        })
    }
}

fn response(event: HubEvent, skipped: u64) -> pb::WatchEventsResponse {
    match event {
        HubEvent::Device(event) => pb::WatchEventsResponse {
            event: event.into(),
            skipped,
            ..Default::default()
        },
        HubEvent::Screen(screen) => pb::WatchEventsResponse {
            screen: screen.into(),
            skipped,
            ..Default::default()
        },
        HubEvent::Activity(activity) => pb::WatchEventsResponse {
            activity: activity.into(),
            skipped,
            ..Default::default()
        },
        HubEvent::Audio(audio) => pb::WatchEventsResponse {
            audio: audio.into(),
            skipped,
            ..Default::default()
        },
        HubEvent::Config(config) => pb::WatchEventsResponse {
            config: config.into(),
            skipped,
            ..Default::default()
        },
        HubEvent::Network(network) => pb::WatchEventsResponse {
            network: buffa::MessageField::some(*network),
            skipped,
            ..Default::default()
        },
        HubEvent::Player(player) => pb::WatchEventsResponse {
            player: player.into(),
            skipped,
            ..Default::default()
        },
        HubEvent::Wake(wake) => pb::WatchEventsResponse {
            wake: wake.into(),
            skipped,
            ..Default::default()
        },
    }
}

/// Most detents one `SendInput` may turn (a fast drag or wheel spin sends a few at once).
const MAX_TURN: i32 = 20;

/// The dock input a `SendInput` request stands for.
fn input_event(request: &pb::SendInputRequest) -> Result<InputEvent, ConnectError> {
    let number = |n: u32| u8::try_from(n).ok();
    match &request.input {
        Some(Input::Key(k)) => {
            let key = number(k.key)
                .and_then(Key::new)
                .ok_or_else(|| ConnectError::invalid_argument(format!("no key {} (keys are 1-9)", k.key)))?;
            Ok(if k.pressed {
                InputEvent::KeyDown(key)
            } else {
                InputEvent::KeyUp(key)
            })
        }
        Some(Input::Knob(k)) => {
            let knob = number(k.knob).and_then(Knob::new).ok_or_else(|| {
                ConnectError::invalid_argument(format!("no knob {} (knobs are 1-3)", k.knob))
            })?;
            match k.action {
                Some(Action::Pressed(true)) => Ok(InputEvent::KnobDown(knob)),
                Some(Action::Pressed(false)) => Ok(InputEvent::KnobUp(knob)),
                Some(Action::Turned(delta)) if delta != 0 && delta.abs() <= MAX_TURN => {
                    Ok(InputEvent::KnobTurn {
                        knob,
                        delta: i8::try_from(delta).expect("within MAX_TURN"),
                    })
                }
                Some(Action::Turned(delta)) => Err(ConnectError::invalid_argument(format!(
                    "turn {delta} out of range (1-{MAX_TURN} detents either way)"
                ))),
                None => Err(ConnectError::invalid_argument("knob event without an action")),
            }
        }
        None => Err(ConnectError::invalid_argument("no input given")),
    }
}

fn lcd_key(key: u32) -> Result<u8, ConnectError> {
    if LCD_KEYS.contains(&key) {
        Ok(u8::try_from(key).expect("1..=6"))
    } else {
        Err(ConnectError::invalid_argument(format!(
            "key {key} has no display (LCD keys are 1-6)"
        )))
    }
}

/// The `ETag` of a key image: a hash of its bytes (H15-94). Content-addressed, so it changes with
/// every new image even when a restarted daemon's counter repeats, and an unchanged image stays
/// cached across restarts. `DefaultHasher::new()` uses fixed keys: the same in every run.
fn image_etag(bytes: &[u8]) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::hash::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("\"{:016x}-{}\"", hasher.finish(), bytes.len())
}

/// `GET /dock/{device}/keys/{key}`: the image an LCD key shows right now, exactly as sent to the
/// dock (upright PNG or JPEG). 404 for blank or unknown keys. Clients add `?v={KeyImage.version}`
/// so caches never serve an old image; the `ETag` (the bytes' hash) lets revalidation skip
/// unchanged ones.
pub fn key_images(hub: Arc<Hub>) -> axum::Router {
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::{IntoResponse, Response};

    async fn key_image(
        State(hub): State<Arc<Hub>>,
        Path((device, key)): Path<(String, u8)>,
        headers: HeaderMap,
    ) -> Response {
        let Some((_, bytes)) = hub.key_image(&device, key) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let etag = image_etag(&bytes);
        if headers
            .get(header::IF_NONE_MATCH)
            .is_some_and(|v| v.as_bytes() == etag.as_bytes())
        {
            return StatusCode::NOT_MODIFIED.into_response();
        }
        let content_type = if bytes.starts_with(b"\x89PNG") {
            "image/png"
        } else {
            "image/jpeg"
        };
        (
            [
                (header::CONTENT_TYPE, content_type.to_owned()),
                (header::ETAG, etag),
                (header::CACHE_CONTROL, "no-cache".to_owned()),
            ],
            bytes.to_vec(),
        )
            .into_response()
    }

    axum::Router::new()
        .route("/dock/{device}/keys/{key}", axum::routing::get(key_image))
        .with_state(hub)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn knob(knob: u32, action: Action) -> pb::SendInputRequest {
        pb::SendInputRequest {
            device_id: "A".into(),
            input: Some(Input::Knob(Box::new(pb::KnobEvent {
                knob,
                action: Some(action),
                ..Default::default()
            }))),
            ..Default::default()
        }
    }

    /// H15-94: a restarted daemon must not answer 304 for a different image under a stale `ETag`,
    /// and an unchanged image may stay cached.
    #[tokio::test]
    async fn key_image_etags_follow_the_content_across_restarts() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode, header};
        use tower::ServiceExt as _;

        async fn show(hub: &Arc<Hub>, image: &[u8]) {
            let mut dock = hub.attach(pb::Device {
                id: "A".into(),
                ..Default::default()
            });
            let reply = tokio::spawn(async move {
                let req = dock.commands.recv().await.unwrap();
                req.reply.send(Ok(())).unwrap();
                dock
            });
            hub.command(
                "A",
                Command::SetKeyImage {
                    key: 1,
                    image: image.to_vec(),
                },
            )
            .await
            .unwrap();
            std::mem::forget(reply.await.unwrap());
        }
        async fn get(hub: &Arc<Hub>, etag: Option<&str>) -> (StatusCode, Option<String>) {
            let mut req = Request::get("/dock/A/keys/1?v=7");
            if let Some(etag) = etag {
                req = req.header(header::IF_NONE_MATCH, etag);
            }
            let res = key_images(Arc::clone(hub))
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            let etag = res
                .headers()
                .get(header::ETAG)
                .map(|v| v.to_str().unwrap().to_owned());
            (res.status(), etag)
        }

        let first = Hub::new();
        show(&first, b"\x89PNG old image").await;
        let (status, etag) = get(&first, None).await;
        assert_eq!(status, StatusCode::OK);
        let old = etag.unwrap();
        assert_eq!(get(&first, Some(&old)).await.0, StatusCode::NOT_MODIFIED);

        // "Restart" (a millisecond later): a new hub shows a different image on the same key.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let second = Hub::new();
        show(&second, b"\x89PNG new image").await;
        let (status, etag) = get(&second, Some(&old)).await;
        assert_eq!(status, StatusCode::OK, "a stale ETag must not keep the old image");
        assert_ne!(etag.unwrap(), old);
        // The same image as before the restart: still cacheable.
        let third = Hub::new();
        show(&third, b"\x89PNG old image").await;
        assert_eq!(get(&third, Some(&old)).await.0, StatusCode::NOT_MODIFIED);
        // And the version (`?v=`) of a new run starts beyond the old one's.
        assert!(third.key_image("A", 1).unwrap().0 > first.key_image("A", 1).unwrap().0);
    }

    #[test]
    fn send_input_maps_and_checks_requests() {
        let key = |key, pressed| pb::SendInputRequest {
            device_id: "A".into(),
            input: Some(Input::Key(Box::new(pb::KeyEvent {
                key,
                pressed,
                ..Default::default()
            }))),
            ..Default::default()
        };
        assert_eq!(
            input_event(&key(9, false)).unwrap(),
            InputEvent::KeyUp(Key::new(9).unwrap())
        );
        assert_eq!(
            input_event(&knob(3, Action::Turned(-4))).unwrap(),
            InputEvent::KnobTurn {
                knob: Knob::new(3).unwrap(),
                delta: -4
            }
        );
        assert_eq!(
            input_event(&knob(1, Action::Pressed(true))).unwrap(),
            InputEvent::KnobDown(Knob::new(1).unwrap())
        );
        for bad in [
            key(0, true),
            key(10, true),
            knob(4, Action::Pressed(true)),
            knob(1, Action::Turned(0)),
            knob(1, Action::Turned(MAX_TURN + 1)),
            pb::SendInputRequest::default(),
        ] {
            assert!(input_event(&bad).is_err(), "{bad:?}");
        }
    }
}
