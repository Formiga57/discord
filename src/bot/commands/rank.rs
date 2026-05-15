use std::sync::Arc;

use serenity::all::{
    Color, CommandInteraction, Context, CreateEmbed, CreateEmbedFooter, CreateInteractionResponse,
    CreateInteractionResponseMessage,
};
use tracing::error;

use crate::store::Store;

pub async fn run(ctx: &Context, command: &CommandInteraction, store: Arc<Store>) {
    let top = store.top_voice(10).await;

    if top.is_empty() {
        reply(ctx, command, "No voice data recorded yet. Join a voice channel!").await;
        return;
    }

    let medals = ["🥇", "🥈", "🥉"];
    let description = top
        .iter()
        .enumerate()
        .map(|(i, (user_id, entry))| {
            let medal = medals.get(i).copied().unwrap_or("▪️");
            let time = format_time(entry.total_ms);
            format!("{medal} <@{user_id}> — **{time}**")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let embed = CreateEmbed::new()
        .title("🏆 Voice Rank")
        .description(description)
        .color(Color::from_rgb(88, 101, 242))
        .footer(CreateEmbedFooter::new("Total accumulated voice channel time"));

    let resp = CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().embed(embed),
    );
    if let Err(e) = command.create_response(&ctx.http, resp).await {
        error!("Failed to send rank response: {e}");
    }
}

fn format_time(ms: u64) -> String {
    let total = ms / 1000;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

async fn reply(ctx: &Context, command: &CommandInteraction, content: &str) {
    let resp = CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(content)
            .ephemeral(true),
    );
    if let Err(e) = command.create_response(&ctx.http, resp).await {
        error!("Failed to send reply: {e}");
    }
}
