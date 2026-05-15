use std::io::{self, Read, Seek, SeekFrom};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use librespot_playback::{
    audio_backend::{Sink, SinkError, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};
use symphonia_core::io::MediaSource;

/// `DiscordSink` is handed to the librespot `Player` as the audio output.
///
/// On every decoded audio packet it converts f64 PCM → f32 LE bytes and
/// pushes them into a bounded crossbeam channel.  The bounded capacity
/// (~1.3 s of audio) back-pressures librespot's decode thread to real-time
/// speed so Spotify doesn't jump to the next track prematurely.
///
/// When librespot stops a track it calls `stop()`, which sends a flush
/// signal so `PcmReader` immediately discards any stale buffered audio from
/// the old track.
pub struct DiscordSink {
    sender: Sender<Vec<u8>>,
    flush_tx: Sender<()>,
}

impl DiscordSink {
    pub fn new(sender: Sender<Vec<u8>>, flush_tx: Sender<()>) -> Self {
        DiscordSink { sender, flush_tx }
    }
}

impl Sink for DiscordSink {
    fn start(&mut self) -> SinkResult<()> {
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        // Signal PcmReader to discard buffered audio from the ending track
        // so the next track starts cleanly without replaying stale samples.
        let _ = self.flush_tx.try_send(());
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = match packet.samples() {
            Ok(s) => s,
            // OGG passthrough packets — skip, we want decoded PCM
            Err(_) => return Ok(()),
        };

        // f64 normalised → f32 normalised (−1.0..=1.0), then to LE bytes.
        // The SoftMixer already applied volume scaling before this point.
        let f32_samples = converter.f64_to_f32(samples);
        let bytes: Vec<u8> = f32_samples.iter().flat_map(|s| s.to_le_bytes()).collect();

        self.sender
            .send(bytes)
            .map_err(|e| SinkError::OnWrite(e.to_string()))
    }
}

// ── PcmReader ──────────────────────────────────────────────────────────────

/// Implements `Read + Seek + MediaSource` so it can be wrapped in
/// `RawAdapter::new(reader, 44_100, 2)` → `Input` for songbird.
///
/// Audio is passed through with zero processing — the Spotify client
/// controls everything (volume, crossfade, normalization) via the
/// SoftMixer/Spirc pipeline.
pub struct PcmReader {
    receiver: Receiver<Vec<u8>>,
    flush_rx: Receiver<()>,
    buf: Vec<u8>,
    pos: usize,
}

impl PcmReader {
    pub fn new(receiver: Receiver<Vec<u8>>, flush_rx: Receiver<()>) -> Self {
        PcmReader { receiver, flush_rx, buf: Vec::new(), pos: 0 }
    }
}

impl Read for PcmReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        // Track transition: discard stale audio so the new track starts
        // immediately without replaying the tail of the previous one.
        if self.flush_rx.try_recv().is_ok() {
            while self.receiver.try_recv().is_ok() {}
            self.buf.clear();
            self.pos = 0;
            out.fill(0);
            return Ok(out.len());
        }

        loop {
            if self.pos < self.buf.len() {
                let available = self.buf.len() - self.pos;
                let to_copy = available.min(out.len());
                out[..to_copy].copy_from_slice(&self.buf[self.pos..self.pos + to_copy]);
                self.pos += to_copy;
                return Ok(to_copy);
            }

            match self.receiver.try_recv() {
                Ok(chunk) => {
                    self.buf = chunk;
                    self.pos = 0;
                }
                // No audio yet (Spotify still loading) — output silence so
                // the track stays alive in the mixer while we wait.
                Err(TryRecvError::Empty) => {
                    out.fill(0);
                    return Ok(out.len());
                }
                // Channel closed (librespot shut down) → signal EOF to songbird.
                Err(TryRecvError::Disconnected) => return Ok(0),
            }
        }
    }
}

impl Seek for PcmReader {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "PcmReader is not seekable"))
    }
}

impl MediaSource for PcmReader {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}
