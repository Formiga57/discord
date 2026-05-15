use std::sync::Arc;

use serenity::all::{CommandInteraction, Context, CreateInteractionResponse, CreateInteractionResponseMessage};
use tracing::info;

use crate::store::Store;

pub async fn run(ctx: &Context, command: &CommandInteraction, store: Arc<Store>) {
    let guild_id = match command.guild_id {
        Some(id) => id,
        None => {
            reply_ephemeral(ctx, command, "This command must be used inside a server.").await;
            return;
        }
    };

    if let Some(session) = store.active_sessions.get(&guild_id) {
        // Signal the session task to shut down cleanly.
        if let Ok(mut guard) = session.shutdown_tx.lock() {
            if let Some(tx) = guard.take() {
                let _ = tx.send(());
            }
        }
        // Drop the guard so remove() can acquire the shard lock.
        drop(session);
        store.active_sessions.remove(&guild_id);

        // Also leave the voice channel via songbird.
        if let Some(manager) = songbird::get(ctx).await {
            let _ = manager.leave(guild_id).await;
        }

        info!("Spotify session stopped manually for guild {guild_id}");
        reply_ephemeral(ctx, command, "✅ Spotify session stopped.").await;
    } else {
        reply_ephemeral(ctx, command, "There is no active Spotify session in this server.").await;
    }
}

async fn reply_ephemeral(ctx: &Context, command: &CommandInteraction, content: &str) {
    let resp = CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(content)
            .ephemeral(true),
    );
    if let Err(e) = command.create_response(&ctx.http, resp).await {
        tracing::error!("Failed to send interaction response: {e}");
    }
}
