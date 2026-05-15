use std::collections::HashMap;

use dashmap::DashMap;
use serenity::model::id::{ChannelId, GuildId, UserId};
use tokio::sync::{oneshot, RwLock};

/// Tokens received after OAuth is complete.
#[derive(Debug, Clone)]
pub struct SpotifyTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
}

/// A running Spotify Connect session for one guild.
pub struct ActiveSession {
    /// Channel where the /start-spotify command was run (for announcements).
    pub text_channel_id: ChannelId,
    /// Voice channel the bot joined.
    pub voice_channel_id: ChannelId,
    /// Send `()` here to request a clean shutdown from /stop-spotify.
    /// Wrapped in Mutex so ActiveSession is Sync (oneshot::Sender is !Sync).
    pub shutdown_tx: std::sync::Mutex<Option<oneshot::Sender<()>>>,
}

/// Per-user voice join timestamp (Instant as ms since UNIX epoch).
#[derive(Debug, Clone)]
pub struct VoiceSession {
    pub joined_at_ms: u128,
    pub display_name: String,
}

/// Persisted voice rank entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct RankEntry {
    pub display_name: String,
    pub total_ms: u64,
}

/// Shared application state threaded through the axum server and Discord bot.
pub struct Store {
    /// Pending OAuth flows: discord_user_id → oneshot sender waiting for tokens.
    pub pending: DashMap<String, oneshot::Sender<SpotifyTokens>>,

    /// The bot's Discord username, set once the gateway Ready event fires.
    pub bot_name: RwLock<String>,

    /// One active Spotify Connect session per guild.
    pub active_sessions: DashMap<GuildId, ActiveSession>,

    /// In-progress voice sessions: user_id → join timestamp.
    pub voice_sessions: DashMap<UserId, VoiceSession>,

    /// Accumulated voice time, persisted to disk on every update.
    pub voice_rank: RwLock<HashMap<String, RankEntry>>,

    /// Path to the JSON rank file.
    pub rank_file: std::path::PathBuf,
}

impl Store {
    pub fn new() -> Self {
        let rank_file = std::path::PathBuf::from("data/voice_rank.json");
        let voice_rank = load_rank(&rank_file);
        Store {
            pending: DashMap::new(),
            bot_name: RwLock::new("Discord Bot".to_string()),
            active_sessions: DashMap::new(),
            voice_sessions: DashMap::new(),
            voice_rank: RwLock::new(voice_rank),
            rank_file,
        }
    }

    /// Record a user joining a voice channel.
    pub fn voice_join(&self, user_id: UserId, display_name: String) {
        let ms = unix_now_ms();
        self.voice_sessions.insert(user_id, VoiceSession { joined_at_ms: ms, display_name });
    }

    /// Record a user leaving a voice channel; accumulate elapsed time.
    pub async fn voice_leave(&self, user_id: UserId) {
        if let Some((_, session)) = self.voice_sessions.remove(&user_id) {
            let elapsed = (unix_now_ms() - session.joined_at_ms) as u64;
            let key = user_id.to_string();
            {
                let mut rank = self.voice_rank.write().await;
                let entry = rank.entry(key).or_default();
                entry.total_ms += elapsed;
                entry.display_name = session.display_name;
            }
            self.persist_rank().await;
        }
    }

    /// Return top N users sorted by total voice time.
    pub async fn top_voice(&self, limit: usize) -> Vec<(String, RankEntry)> {
        let rank = self.voice_rank.read().await;
        let mut entries: Vec<_> = rank.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        entries.sort_by(|a, b| b.1.total_ms.cmp(&a.1.total_ms));
        entries.truncate(limit);
        entries
    }

    async fn persist_rank(&self) {
        if let Some(parent) = self.rank_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let rank = self.voice_rank.read().await;
        if let Ok(json) = serde_json::to_string_pretty(&*rank) {
            let _ = std::fs::write(&self.rank_file, json);
        }
    }
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

fn load_rank(path: &std::path::Path) -> HashMap<String, RankEntry> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn unix_now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
