pub mod commands;

use std::sync::Arc;

use serenity::{
    all::{
        Client, CreateCommand, GatewayIntents, Interaction, Ready, VoiceState,
    },
    async_trait,
    prelude::{Context, EventHandler},
};
use songbird::SerenityInit;
use tracing::{error, info};

use crate::{config::Config, store::Store};

struct Handler {
    cfg: Arc<Config>,
    store: Arc<Store>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("{} is connected and ready", ready.user.name);

        *self.store.bot_name.write().await = ready.user.name.clone();

        let guild_id = serenity::model::id::GuildId::new(self.cfg.discord_guild_id);

        let slash_commands = vec![
            CreateCommand::new("start-spotify")
                .description("Connect your Spotify and start playing in your voice channel"),
            CreateCommand::new("stop-spotify")
                .description("Stop the active Spotify Connect session"),
            CreateCommand::new("rank")
                .description("Show the voice channel time leaderboard"),
        ];

        match guild_id.set_commands(&ctx.http, slash_commands).await {
            Ok(cmds) => info!("Registered {} slash commands", cmds.len()),
            Err(e) => error!("Failed to register commands: {e}"),
        }
    }

    async fn voice_state_update(&self, ctx: Context, old: Option<VoiceState>, new: VoiceState) {
        if new.user_id == ctx.cache.current_user().id {
            return;
        }

        let user_id = new.user_id;
        let display_name = new
            .member
            .as_ref()
            .map(|m| m.display_name().to_string())
            .unwrap_or_else(|| user_id.to_string());

        let was_in_channel = old.as_ref().and_then(|v| v.channel_id).is_some();
        let is_in_channel = new.channel_id.is_some();

        if !was_in_channel && is_in_channel {
            self.store.voice_join(user_id, display_name);
        } else if was_in_channel && !is_in_channel {
            self.store.voice_leave(user_id).await;
        } else if was_in_channel && is_in_channel {
            // Channel switch — restart session timer
            self.store.voice_leave(user_id).await;
            self.store.voice_join(user_id, display_name);
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            match command.data.name.as_str() {
                "start-spotify" => {
                    commands::start_spotify::run(
                        &ctx,
                        &command,
                        self.cfg.clone(),
                        self.store.clone(),
                    )
                    .await;
                }
                "stop-spotify" => {
                    commands::stop_spotify::run(&ctx, &command, self.store.clone()).await;
                }
                "rank" => {
                    commands::rank::run(&ctx, &command, self.store.clone()).await;
                }
                _ => {}
            }
        }
    }
}

pub async fn build_client(cfg: Arc<Config>, store: Arc<Store>) -> anyhow::Result<Client> {
    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_VOICE_STATES
        | GatewayIntents::GUILD_MESSAGES;

    let client = Client::builder(&cfg.discord_token, intents)
        .event_handler(Handler { cfg, store })
        .register_songbird()
        .await?;

    Ok(client)
}
