use std::{sync::Arc, time::Duration};

use librespot_playback::player::PlayerEvent;
use serenity::all::{
    ActivityData, Color, CommandInteraction, Context, CreateActionRow, CreateButton, CreateEmbed,
    CreateEmbedAuthor, CreateEmbedFooter, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, OnlineStatus,
};
use songbird::input::{Input, RawAdapter};
use tokio::sync::oneshot;
use tracing::{debug, error, info};

use crate::{
    config::Config,
    spotify::{
        auth::{build_auth_url, get_spotify_user},
        connect::create_connect_device,
    },
    store::{ActiveSession, Store},
};

pub async fn run(ctx: &Context, command: &CommandInteraction, cfg: Arc<Config>, store: Arc<Store>) {
    // ── 1. Require a voice channel ────────────────────────────────────────
    let guild_id = match command.guild_id {
        Some(id) => id,
        None => {
            reply_ephemeral(ctx, command, "This command must be used inside a server.").await;
            return;
        }
    };

    let voice_ch_id = ctx
        .cache
        .guild(guild_id)
        .and_then(|g| g.voice_states.get(&command.user.id).and_then(|vs| vs.channel_id));

    let voice_ch_id = match voice_ch_id {
        Some(id) => id,
        None => {
            reply_ephemeral(ctx, command, "You need to be in a voice channel first.").await;
            return;
        }
    };

    // ── 2. Only one session per guild ────────────────────────────────────
    if store.active_sessions.contains_key(&guild_id) {
        reply_ephemeral(
            ctx,
            command,
            "A Spotify session is already active in this server. Use `/stop-spotify` first.",
        )
        .await;
        return;
    }

    let text_ch_id = command.channel_id;

    // ── 3. Send ephemeral auth link ──────────────────────────────────────
    let user_id = command.user.id.to_string();
    let auth_url = build_auth_url(&cfg, &user_id);

    let msg = format!(
        "**Connect your Spotify account:**\n[Click here to authorize]({auth_url})\n\n\
         _This link expires in 5 minutes._"
    );
    reply_ephemeral(ctx, command, &msg).await;

    // ── 4. Wait for OAuth callback (5-minute timeout) ─────────────────────
    let (tx, rx) = oneshot::channel();
    store.pending.insert(user_id.clone(), tx);

    let tokens = match tokio::time::timeout(Duration::from_secs(300), rx).await {
        Ok(Ok(t)) => t,
        Ok(Err(_)) => {
            store.pending.remove(&user_id);
            follow_up(ctx, command, "❌ Authorization was cancelled.").await;
            return;
        }
        Err(_) => {
            store.pending.remove(&user_id);
            follow_up(ctx, command, "❌ Authorization timed out. Please run the command again.").await;
            return;
        }
    };

    // ── 5. Resolve Spotify display name ──────────────────────────────────
    let spotify_name = get_spotify_user(&tokens.access_token)
        .await
        .unwrap_or_else(|_| "Unknown".to_string());
    info!("Spotify user: {spotify_name} — joining voice channel {voice_ch_id}");

    follow_up(
        ctx,
        command,
        &format!("✅ Connected as **{spotify_name}**. Joining your voice channel…"),
    )
    .await;

    // ── 6. Create librespot Spirc device + PcmReader ──────────────────────
    let device_name = store.bot_name.read().await.clone();
    let (spirc, reader, event_channel, jam_url_rx) =
        match create_connect_device(&device_name, &tokens.access_token).await {
            Ok(pair) => pair,
            Err(e) => {
                error!("Failed to create Spotify Connect device: {e}");
                follow_up(ctx, command, &format!("❌ Spotify Connect error: {e}")).await;
                return;
            }
        };

    // ── 7. Join voice channel ────────────────────────────────────────────
    let manager = match songbird::get(ctx).await {
        Some(m) => m,
        None => {
            error!("Songbird not initialised");
            follow_up(ctx, command, "❌ Internal error: voice system not ready.").await;
            let _ = spirc.shutdown();
            return;
        }
    };

    let handler_lock = match manager.join(guild_id, voice_ch_id).await {
        Ok(h) => h,
        Err(e) => {
            error!("Failed to join voice channel: {e}");
            follow_up(ctx, command, &format!("❌ Could not join voice channel: {e}")).await;
            let _ = spirc.shutdown();
            return;
        }
    };

    let input: Input = RawAdapter::new(reader, 44_100, 2).into();
    {
        let mut handler = handler_lock.lock().await;
        handler.play_input(input);
    }

    follow_up(
        ctx,
        command,
        &format!(
            "✅ Connected as **{spotify_name}**. Select **{device_name}** in Spotify — I'll stream audio here!\n\
             Hit **Start a Jam** in Spotify and I'll post the invite link."
        ),
    )
    .await;

    // ── 8. Register active session (enables /stop-spotify) ───────────────
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    store.active_sessions.insert(
        guild_id,
        ActiveSession {
            text_channel_id: text_ch_id,
            voice_channel_id: voice_ch_id,
            shutdown_tx: std::sync::Mutex::new(Some(shutdown_tx)),
        },
    );

    // ── 9. Jam URL announcements ─────────────────────────────────────────
    let ctx_jam = ctx.clone();
    let bot_name_jam = device_name.clone();
    tokio::spawn(async move {
        let mut rx = jam_url_rx;
        let mut sent_tokens = std::collections::HashSet::new();
        while let Some(url) = rx.recv().await {
            let token = url
                .strip_prefix("spotify://socialsession/")
                .unwrap_or(&url)
                .to_owned();

            if !sent_tokens.insert(token.clone()) {
                debug!("Jam URL already sent, skipping: {url}");
                continue;
            }

            info!("Jam session started: {url}");

            let https_url = format!("https://open.spotify.com/socialsession/{token}");
            let embed = CreateEmbed::new()
                .title("A Jam just started!")
                .description(format!(
                    "**{bot_name_jam}** is hosting a listening session.\n\
                    Join and listen along in sync with everyone."
                ))
                .color(Color::from_rgb(30, 215, 96))
                .footer(CreateEmbedFooter::new("Spotify Jam"));

            let button = CreateButton::new_link(&https_url).label("Join the Jam");

            let _ = text_ch_id
                .send_message(
                    &ctx_jam.http,
                    CreateMessage::new()
                        .embed(embed)
                        .components(vec![CreateActionRow::Buttons(vec![button])]),
                )
                .await;
        }
    });

    // ── 10. Now-playing embed + presence on every track change ────────────
    let ctx_np = ctx.clone();
    let device_name_np = device_name.clone();
    tokio::spawn(async move {
        let mut events = event_channel;
        while let Some(event) = events.recv().await {
            match event {
                PlayerEvent::TrackChanged { audio_item } => {
                    let track = audio_item.name.clone();

                    let (artist, album) = match &audio_item.unique_fields {
                        librespot_metadata::audio::UniqueFields::Track { artists, album, .. } => (
                            artists.0.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "),
                            album.clone(),
                        ),
                        librespot_metadata::audio::UniqueFields::Local { artists, album, .. } => (
                            artists.as_deref().unwrap_or("Unknown artist").to_string(),
                            album.as_deref().unwrap_or("").to_string(),
                        ),
                        librespot_metadata::audio::UniqueFields::Episode { show_name, .. } => {
                            (show_name.clone(), String::new())
                        }
                    };

                    let cover_url = audio_item
                        .covers
                        .iter()
                        .max_by_key(|c| c.width)
                        .map(|c| c.url.clone())
                        .unwrap_or_default();

                    let spotify_url = audio_item
                        .uri
                        .strip_prefix("spotify:")
                        .map(|rest| format!("https://open.spotify.com/{}", rest.replacen(':', "/", 1)))
                        .unwrap_or_default();

                    let duration = {
                        let total = audio_item.duration_ms / 1000;
                        format!("{}:{:02}", total / 60, total % 60)
                    };

                    let explicit_tag = if audio_item.is_explicit { " 🅴" } else { "" };
                    let status = format!("{track} · {artist}");
                    info!("Now playing: {status}");

                    ctx_np.set_presence(
                        Some(ActivityData::listening(&status)),
                        OnlineStatus::Online,
                    );

                    let album_url = if !album.is_empty() {
                        let mut u = url::Url::parse("https://open.spotify.com/search/").unwrap();
                        u.path_segments_mut().unwrap().push(&album);
                        u.to_string()
                    } else {
                        String::new()
                    };

                    let mut embed = CreateEmbed::new()
                        .author(CreateEmbedAuthor::new(&artist))
                        .title(format!("{track}{explicit_tag}"))
                        .color(Color::from_rgb(30, 215, 96))
                        .footer(CreateEmbedFooter::new(format!(
                            "🎧 {device_name_np}  ·  {duration}"
                        )));

                    if !spotify_url.is_empty() {
                        embed = embed.url(&spotify_url);
                    }
                    if !album.is_empty() {
                        let album_desc = if !album_url.is_empty() {
                            format!("[*{album}*]({album_url})")
                        } else {
                            format!("*{album}*")
                        };
                        embed = embed.description(album_desc);
                    }
                    if !cover_url.is_empty() {
                        embed = embed.thumbnail(&cover_url);
                    }

                    let _ = text_ch_id
                        .send_message(&ctx_np.http, CreateMessage::new().embed(embed))
                        .await;
                }
                PlayerEvent::Stopped { .. } => {
                    ctx_np.set_presence(None, OnlineStatus::Online);
                }
                _ => {}
            }
        }
        ctx_np.set_presence(None, OnlineStatus::Online);
    });

    // ── 11. Session watchdog: clean up on channel empty OR /stop-spotify ──
    let ctx2 = ctx.clone();
    let store2 = store.clone();
    tokio::spawn(async move {
        let mut shutdown_rx = shutdown_rx;
        loop {
            tokio::select! {
                // External shutdown request from /stop-spotify
                _ = &mut shutdown_rx => {
                    info!("Spotify session shut down by /stop-spotify command");
                    break;
                }
                // Poll every 5 s for an empty voice channel
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    let still_connected = ctx2
                        .cache
                        .guild(guild_id)
                        .map(|g| {
                            g.voice_states.values().any(|vs| {
                                vs.channel_id == Some(voice_ch_id)
                                    && vs.user_id != ctx2.cache.current_user().id
                            })
                        })
                        .unwrap_or(false);

                    if !still_connected {
                        info!("Voice channel empty — shutting down Spotify Connect device");
                        break;
                    }
                }
            }
        }

        let _ = spirc.shutdown();
        let _ = manager.leave(guild_id).await;
        store2.active_sessions.remove(&guild_id);
    });
}

async fn reply_ephemeral(ctx: &Context, command: &CommandInteraction, content: &str) {
    let resp = CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(content)
            .ephemeral(true),
    );
    if let Err(e) = command.create_response(&ctx.http, resp).await {
        error!("Failed to send interaction response: {e}");
    }
}

async fn follow_up(ctx: &Context, command: &CommandInteraction, content: &str) {
    use serenity::all::CreateInteractionResponseFollowup;
    let resp = CreateInteractionResponseFollowup::new()
        .content(content)
        .ephemeral(true);
    if let Err(e) = command.create_followup(&ctx.http, resp).await {
        error!("Failed to send follow-up: {e}");
    }
}
