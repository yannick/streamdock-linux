//! Moves a Lyrion queue from one player to another without syncing them (H15-91): a sync makes
//! the target start the group's stream, which the built-in player once couldn't decode.
//!
//! 1. The source pauses; its track, position, shuffle and repeat are read.
//! 2. Its queue is saved as a hidden playlist (`zz-streamdock-transfer-<mac>`), which is deleted
//!    again in every case.
//! 3. The target loads it (shuffle off, so the saved order stays), jumps to the same track and
//!    position, gets repeat and shuffle back, and plays if the source played.
//! 4. Only when the target is on that track is the source cleared and switched off; otherwise
//!    the target is cleared and the source plays on.

use std::future::Future;
use std::time::Duration;

use anyhow::{Context as _, bail};
use serde_json::{Value, json};

/// How long the target may take to start the track.
#[cfg(not(test))]
const SETTLE: Duration = Duration::from_secs(5);
#[cfg(test)]
const SETTLE: Duration = Duration::from_millis(600);
const POLL: Duration = Duration::from_millis(250);

/// One Lyrion command for a player (`""` for the server).
pub trait Lms: Sync {
    fn request(&self, player: &str, command: Value) -> impl Future<Output = anyhow::Result<Value>> + Send;
}

impl Lms for crate::modules::lyrion::client::Client {
    async fn request(&self, player: &str, command: Value) -> anyhow::Result<Value> {
        crate::modules::lyrion::client::Client::request(self, player, command.clone())
            .await
            .with_context(|| format!("{player}: {command}"))
    }
}

/// What the source was doing.
#[derive(Debug, Clone, PartialEq)]
struct Position {
    playing: bool,
    /// The current track's id.
    track: Option<i64>,
    index: usize,
    seconds: f64,
    shuffle: i64,
    repeat: i64,
    /// The length of the queue.
    tracks: usize,
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn position(status: &Value, playing: bool) -> Position {
    let int = |key: &str| {
        number(&status[key]).map_or(0, |n| {
            #[allow(clippy::cast_possible_truncation, reason = "Lyrion's small enums and indexes")]
            let i = n as i64;
            i
        })
    };
    Position {
        playing,
        track: number(&status["playlist_loop"][0]["id"]).map(|n| {
            #[allow(clippy::cast_possible_truncation, reason = "Lyrion's track ids")]
            let id = n as i64;
            id
        }),
        index: usize::try_from(int("playlist_cur_index")).unwrap_or(0),
        seconds: number(&status["time"]).unwrap_or(0.0),
        shuffle: int("playlist shuffle"),
        repeat: int("playlist repeat"),
        tracks: usize::try_from(int("playlist_tracks")).unwrap_or(0),
    }
}

/// The ids of a player's queue, in order.
fn track_ids(status: &Value) -> Vec<Option<i64>> {
    status["playlist_loop"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|t| {
                    number(&t["id"]).map(|n| {
                        #[allow(clippy::cast_possible_truncation, reason = "Lyrion's track ids")]
                        let id = n as i64;
                        id
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Where `track` is in `ids`: at `hint` if it is there (the same track can be queued twice),
/// else its first place; `hint` when the source had no track id.
fn find(ids: &[Option<i64>], track: Option<i64>, hint: usize) -> Option<usize> {
    let Some(track) = track else {
        return (hint < ids.len()).then_some(hint);
    };
    if ids.get(hint) == Some(&Some(track)) {
        return Some(hint);
    }
    ids.iter().position(|id| *id == Some(track))
}

/// The hidden playlist's name for a move to `to`.
fn temporary(to: &str) -> String {
    format!("zz-streamdock-transfer-{}", to.replace(':', ""))
}

/// Moves the queue from `from` to `to`.
pub async fn move_queue(lms: &impl Lms, from: &str, to: &str) -> anyhow::Result<()> {
    let mode = lms.request(from, json!(["mode", "?"])).await?;
    let playing = mode["_mode"] == "play";
    if playing {
        lms.request(from, json!(["pause", "1"])).await?;
    }
    let status = lms.request(from, json!(["status", "-", "1", "tags:"])).await?;
    let at = position(&status, playing);
    let saved = lms
        .request(from, json!(["playlist", "save", temporary(to), "silent:1"]))
        .await;
    let result = match saved.as_ref().ok().and_then(|s| number(&s["__playlist_id"])) {
        Some(id) => load(lms, to, id, &at).await,
        None => Err(anyhow::anyhow!("Lyrion didn't save the queue: {saved:?}")),
    };
    if let Ok(saved) = &saved
        && let Some(id) = number(&saved["__playlist_id"])
    {
        let delete = lms
            .request("", json!(["playlists", "delete", format!("playlist_id:{id}")]))
            .await;
        if let Err(e) = delete {
            tracing::warn!(error = %format!("{e:#}"), "the transfer playlist wasn't deleted");
        }
    }
    match result {
        Ok(()) => {
            lms.request(from, json!(["playlist", "clear"])).await?;
            lms.request(from, json!(["power", "0"])).await?;
            Ok(())
        }
        Err(e) => {
            // The music stays where it was.
            drop(lms.request(to, json!(["playlist", "clear"])).await);
            if playing {
                drop(lms.request(from, json!(["pause", "0"])).await);
            }
            Err(e)
        }
    }
}

/// Loads the saved queue on `to` and puts it where the source was.
async fn load(lms: &impl Lms, to: &str, playlist: f64, at: &Position) -> anyhow::Result<()> {
    lms.request(to, json!(["power", "1"])).await?;
    lms.request(to, json!(["playlist", "shuffle", "0"])).await?;
    lms.request(to, json!(["playlist", "clear"])).await?;
    lms.request(
        to,
        json!(["playlistcontrol", "cmd:add", format!("playlist_id:{playlist}")]),
    )
    .await?;
    lms.request(to, json!(["playlist", "repeat", at.repeat.to_string()]))
        .await?;
    let queue = lms.request(to, json!(["status", "0", "9999", "tags:"])).await?;
    let ids = track_ids(&queue);
    if ids.len() != at.tracks {
        bail!("only {} of {} tracks arrived", ids.len(), at.tracks);
    }
    let index = find(&ids, at.track, at.index).context("the track isn't in the moved queue")?;
    if at.playing {
        lms.request(to, json!(["playlist", "index", index.to_string()]))
            .await?;
        lms.request(to, json!(["time", format!("{:.1}", at.seconds)]))
            .await?;
    } else {
        // Not playing: the track without a sound; the position within it is lost.
        lms.request(to, json!(["playlist", "index", index.to_string(), "", "1"]))
            .await?;
    }
    if at.shuffle != 0 {
        lms.request(to, json!(["playlist", "shuffle", at.shuffle.to_string()]))
            .await?;
    }
    settled(lms, to, at).await
}

/// Waits until `to` is on the source's track (and plays, if the source played).
async fn settled(lms: &impl Lms, to: &str, at: &Position) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        let status = lms.request(to, json!(["status", "-", "1", "tags:"])).await?;
        let now = position(&status, status["mode"] == "play");
        let on_track = at.track.is_none() || now.track == at.track;
        if on_track && (now.playing || !at.playing) {
            return Ok(());
        }
        if tokio::time::Instant::now() > deadline {
            bail!("the target didn't start the track");
        }
        tokio::time::sleep(POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A Lyrion with two players' queues, enough of it for a move.
    #[derive(Debug, Default)]
    struct FakeLms {
        players: Mutex<std::collections::BTreeMap<String, Player>>,
        playlists: Mutex<Vec<(i64, Vec<i64>)>>,
        log: Mutex<Vec<String>>,
        /// The target never starts playing.
        stuck: bool,
        /// Saved playlists lose their last track.
        truncate: bool,
    }

    #[derive(Debug, Default, Clone)]
    struct Player {
        queue: Vec<i64>,
        index: usize,
        mode: &'static str,
        time: f64,
        power: bool,
        shuffle: i64,
        repeat: i64,
    }

    impl FakeLms {
        fn with_source() -> Self {
            let lms = Self::default();
            lms.players.lock().unwrap().insert(
                "speaker".into(),
                Player {
                    queue: vec![10, 11, 12, 13],
                    index: 2,
                    mode: "play",
                    time: 42.5,
                    power: true,
                    shuffle: 0,
                    repeat: 2,
                },
            );
            lms.players
                .lock()
                .unwrap()
                .insert("luckfox".into(), Player::default());
            lms
        }

        fn player(&self, id: &str) -> Player {
            self.players.lock().unwrap()[id].clone()
        }

        fn answer(&self, player: &str, args: &[String]) -> Value {
            let mut players = self.players.lock().unwrap();
            let words: Vec<&str> = args.iter().map(String::as_str).collect();
            if words.first() == Some(&"playlists") {
                let id: i64 = words[2].trim_start_matches("playlist_id:").parse().unwrap();
                self.playlists.lock().unwrap().retain(|(p, _)| *p != id);
                return json!({});
            }
            let p = players.get_mut(player).unwrap();
            match words.as_slice() {
                ["mode", "?"] => json!({ "_mode": p.mode }),
                ["pause", flag] => {
                    p.mode = if *flag == "1" { "pause" } else { "play" };
                    json!({})
                }
                ["status", "-", ..] => json!({
                    "mode": p.mode, "time": p.time, "playlist_cur_index": p.index.to_string(),
                    "playlist_tracks": p.queue.len(),
                    "playlist shuffle": p.shuffle, "playlist repeat": p.repeat,
                    "playlist_loop": p.queue.get(p.index).map(|id| vec![json!({ "id": id })]).unwrap_or_default(),
                }),
                ["status", "0", ..] => {
                    json!({ "playlist_loop": p.queue.iter().map(|id| json!({ "id": id })).collect::<Vec<_>>() })
                }
                ["playlist", "save", ..] => {
                    let mut lists = self.playlists.lock().unwrap();
                    let mut saved = p.queue.clone();
                    if self.truncate {
                        saved.pop();
                    }
                    lists.push((99, saved));
                    json!({ "__playlist_id": 99 })
                }
                ["playlist", "clear"] => {
                    p.queue.clear();
                    p.mode = "stop";
                    json!({})
                }
                ["playlistcontrol", "cmd:add", id] => {
                    let id: f64 = id.trim_start_matches("playlist_id:").parse().unwrap();
                    #[allow(clippy::cast_possible_truncation)]
                    let id = id as i64;
                    let lists = self.playlists.lock().unwrap();
                    p.queue
                        .extend(lists.iter().find(|(l, _)| *l == id).unwrap().1.clone());
                    json!({})
                }
                ["playlist", "index", i, rest @ ..] => {
                    p.index = i.parse().unwrap();
                    p.mode = if rest.is_empty() && !self.stuck {
                        "play"
                    } else {
                        "stop"
                    };
                    json!({})
                }
                ["time", t] => {
                    p.time = t.parse().unwrap();
                    json!({})
                }
                ["power", on] => {
                    p.power = *on == "1";
                    json!({})
                }
                ["playlist", "shuffle", s] => {
                    p.shuffle = s.parse().unwrap();
                    json!({})
                }
                ["playlist", "repeat", r] => {
                    p.repeat = r.parse().unwrap();
                    json!({})
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    impl Lms for FakeLms {
        async fn request(&self, player: &str, command: Value) -> anyhow::Result<Value> {
            tokio::task::yield_now().await;
            let args: Vec<String> = command
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                .collect();
            self.log
                .lock()
                .unwrap()
                .push(format!("{player} {}", args.join(" ")));
            Ok(self.answer(player, &args))
        }
    }

    #[tokio::test]
    async fn a_playing_queue_moves_with_its_track_position_and_modes() {
        let lms = FakeLms::with_source();
        move_queue(&lms, "speaker", "luckfox").await.unwrap();
        let to = lms.player("luckfox");
        assert_eq!(
            (to.queue.as_slice(), to.index, to.mode),
            ([10, 11, 12, 13].as_slice(), 2, "play")
        );
        assert!((to.time - 42.5).abs() < 0.01, "the position: {}", to.time);
        assert_eq!((to.repeat, to.power), (2, true));
        let from = lms.player("speaker");
        assert!(
            from.queue.is_empty() && !from.power,
            "the source is cleared and off"
        );
        assert!(
            lms.playlists.lock().unwrap().is_empty(),
            "the hidden playlist is deleted"
        );
        let log = lms.log.lock().unwrap();
        let first_clear = log.iter().position(|l| l == "speaker playlist clear").unwrap();
        let play = log
            .iter()
            .position(|l| l.starts_with("luckfox playlist index"))
            .unwrap();
        assert!(
            play < first_clear,
            "the source is cleared only after the target plays"
        );
        assert!(log.iter().any(|l| l.contains("zz-streamdock-transfer-luckfox")));
    }

    #[tokio::test]
    async fn a_paused_queue_moves_without_a_sound() {
        let lms = FakeLms::with_source();
        lms.players.lock().unwrap().get_mut("speaker").unwrap().mode = "pause";
        move_queue(&lms, "speaker", "luckfox").await.unwrap();
        let to = lms.player("luckfox");
        assert_eq!((to.index, to.mode), (2, "stop"), "on the track, not playing");
        assert!(!lms.log.lock().unwrap().iter().any(|l| l == "speaker pause 1"));
    }

    #[tokio::test]
    async fn when_the_target_fails_the_music_stays_on_the_source() {
        let lms = FakeLms {
            stuck: true,
            ..FakeLms::with_source()
        };
        let err = move_queue(&lms, "speaker", "luckfox").await.unwrap_err();
        assert!(format!("{err:#}").contains("didn't start"), "{err:#}");
        let from = lms.player("speaker");
        assert_eq!(
            (from.queue.len(), from.mode, from.power),
            (4, "play", true),
            "playing on"
        );
        assert_eq!(lms.player("luckfox").queue.len(), 0, "the target is cleared");
        assert!(lms.playlists.lock().unwrap().is_empty(), "deleted on error too");
    }

    #[tokio::test]
    async fn a_queue_that_arrives_incomplete_stays_on_the_source() {
        let lms = FakeLms::with_source();
        // Lyrion saved only part of it.
        lms.players
            .lock()
            .unwrap()
            .get_mut("speaker")
            .unwrap()
            .queue
            .push(14);
        lms.playlists.lock().unwrap().clear();
        let lost = FakeLms {
            players: Mutex::new(lms.players.lock().unwrap().clone()),
            truncate: true,
            ..FakeLms::default()
        };
        let err = move_queue(&lost, "speaker", "luckfox").await.unwrap_err();
        assert!(format!("{err:#}").contains("4 of 5 tracks"), "{err:#}");
        assert_eq!(
            lost.player("speaker").queue.len(),
            5,
            "the source keeps its queue"
        );
        assert_eq!(lost.player("speaker").mode, "play");
    }

    #[test]
    fn the_track_is_found_at_its_index_or_else_first() {
        let ids = [Some(1), Some(2), Some(1), Some(3)];
        assert_eq!(
            find(&ids, Some(1), 2),
            Some(2),
            "a track queued twice: the source's place"
        );
        assert_eq!(find(&ids, Some(3), 0), Some(3));
        assert_eq!(find(&ids, Some(9), 0), None);
        assert_eq!(find(&ids, None, 1), Some(1));
        assert_eq!(
            temporary("02:00:00:00:00:ee"),
            "zz-streamdock-transfer-0200000000ee"
        );
    }
}
