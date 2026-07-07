//! Voice-prompt prototype (issue #58): push-to-talk mic capture → local,
//! open-source Whisper transcription → text handed back to the steer strip
//! (the human reviews it and presses Enter; loomux never auto-submits).
//!
//! ## Why this shape
//!
//! Two axes had to clear the Windows-10 baseline constraint (no getrandom /
//! ProcessPrng in the shipped binary — see Cargo.toml) *and* keep the repo's
//! `cargo check` gate cheap:
//!
//! * **Capture — `cpal` (native WASAPI).** Vetted getrandom-clean at runtime
//!   (`cargo tree -i getrandom --target all` shows getrandom only under
//!   build-dependencies / Android's oboe-sys, never in cpal's Windows runtime
//!   tree). Native capture also sidesteps the WebView2 `getUserMedia`
//!   microphone-permission dance, so the demo is deterministic. cpal is pure
//!   Rust WASAPI bindings — no C++/cmake build.
//! * **Transcription — subprocess to a prebuilt whisper.cpp `whisper-cli.exe`**
//!   (git.rs-style `Command`), NOT the `whisper-rs` crate. whisper-rs is itself
//!   getrandom-clean at runtime, but `whisper-rs-sys` builds whisper.cpp from
//!   C++ via cmake, which would drag a cmake + MSVC toolchain requirement into
//!   `cargo check --locked` for everyone. Shelling out keeps the gate a pure-
//!   Rust check and matches how git.rs already integrates an external CLI.
//!
//! The `whisper-cli.exe` binary and the model weights are NOT committed — they
//! are downloaded once (see doc/demo/voice-prompt.md) and discovered at runtime
//! by [`resolve_whisper`].
//!
//! ## Flow
//!
//! `voice_start` opens the default input device on a dedicated thread that owns
//! the (non-`Send`) cpal stream and appends mono f32 samples to a shared buffer.
//! `voice_stop` signals that thread, joins it, resamples the captured audio to
//! the 16 kHz mono PCM Whisper expects, writes a scratch WAV, runs whisper.cpp,
//! and returns the transcript. Pure helpers ([`resample_linear`],
//! [`encode_wav_pcm16`], [`parse_whisper_output`]) are unit-tested below.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Sample rate Whisper (ggml) models are trained on. Everything is resampled to
/// this mono rate before transcription.
const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// Audio captured by the recording thread: interleaved-then-downmixed mono f32
/// at the device's native rate (resampled to 16 kHz only at stop time).
struct Captured {
    samples: Vec<f32>,
    sample_rate: u32,
}

/// A recording in flight. Dropping the [`Sender`] (or sending on it) tells the
/// capture thread to stop; joining yields the captured audio. The cpal stream
/// itself lives entirely inside that thread because it is `!Send` on Windows.
struct Recording {
    stop_tx: Sender<()>,
    join: JoinHandle<Captured>,
}

/// Managed Tauri state: at most one active recording at a time.
#[derive(Default)]
pub struct VoiceState(Mutex<Option<Recording>>);

/// Begin capturing from the default input device. Errors immediately (before
/// returning) if there is no input device or the stream can't be built, so the
/// UI can surface a real message instead of a silent no-op.
#[tauri::command]
pub fn voice_start(state: tauri::State<'_, VoiceState>) -> Result<(), String> {
    let mut slot = state.0.lock().map_err(|_| "voice state poisoned")?;
    if slot.is_some() {
        return Err("already recording".into());
    }

    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    // The thread reports stream-build success/failure back here so voice_start
    // can fail synchronously with a useful message.
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

    let join = std::thread::spawn(move || capture_loop(stop_rx, ready_tx));

    match ready_rx.recv() {
        Ok(Ok(())) => {
            *slot = Some(Recording { stop_tx, join });
            Ok(())
        }
        // Build failed: the thread already returned; join it to avoid a leak.
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(_) => {
            let _ = join.join();
            Err("recording thread died before start".into())
        }
    }
}

/// Stop the active recording, transcribe it locally, and return the text.
/// Empty/near-silent captures return an empty string rather than an error, so a
/// mis-click just yields nothing to insert.
#[tauri::command]
pub fn voice_stop(state: tauri::State<'_, VoiceState>) -> Result<String, String> {
    let recording = {
        let mut slot = state.0.lock().map_err(|_| "voice state poisoned")?;
        slot.take().ok_or("not recording")?
    };
    // Signal stop (ignore error: if the thread already exited on device loss,
    // the join below still yields whatever it captured).
    let _ = recording.stop_tx.send(());
    let captured = recording
        .join
        .join()
        .map_err(|_| "recording thread panicked".to_string())?;

    if captured.samples.is_empty() {
        return Ok(String::new());
    }
    let mono16k = resample_linear(&captured.samples, captured.sample_rate, WHISPER_SAMPLE_RATE);
    transcribe(&mono16k)
}

/// Cancel any active recording without transcribing (e.g. the pane closed).
/// Idempotent — a no-op when nothing is recording.
#[tauri::command]
pub fn voice_cancel(state: tauri::State<'_, VoiceState>) -> Result<(), String> {
    let recording = {
        let mut slot = state.0.lock().map_err(|_| "voice state poisoned")?;
        slot.take()
    };
    if let Some(r) = recording {
        let _ = r.stop_tx.send(());
        let _ = r.join.join();
    }
    Ok(())
}

/// Body of the capture thread. Builds the input stream, reports readiness, then
/// blocks until stopped, appending downmixed-mono f32 samples to the buffer it
/// returns. The cpal `Stream` never leaves this thread (it is `!Send`).
fn capture_loop(stop_rx: Receiver<()>, ready_tx: Sender<Result<(), String>>) -> Captured {
    let host = cpal::default_host();
    let device = match host.default_input_device() {
        Some(d) => d,
        None => {
            let _ = ready_tx.send(Err("no microphone / input device found".into()));
            return Captured { samples: Vec::new(), sample_rate: WHISPER_SAMPLE_RATE };
        }
    };
    let supported = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("no default input config: {e}")));
            return Captured { samples: Vec::new(), sample_rate: WHISPER_SAMPLE_RATE };
        }
    };

    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
    let err_fn = |e| eprintln!("voice: input stream error: {e}");

    // One closure shape per sample format: downmix each frame to a single mono
    // sample (average of channels) and append. Whisper is mono anyway.
    macro_rules! build {
        ($t:ty, $to_f32:expr) => {{
            let buf = buffer.clone();
            device.build_input_stream(
                &config,
                move |data: &[$t], _: &cpal::InputCallbackInfo| {
                    let to_f32 = $to_f32;
                    let mut b = buf.lock().unwrap();
                    for frame in data.chunks(channels.max(1)) {
                        let sum: f32 = frame.iter().map(|&s| to_f32(s)).sum();
                        b.push(sum / frame.len().max(1) as f32);
                    }
                },
                err_fn,
                None,
            )
        }};
    }

    let stream = match sample_format {
        cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => build!(i16, |s: i16| s as f32 / 32768.0),
        cpal::SampleFormat::U16 => build!(u16, |s: u16| (s as f32 - 32768.0) / 32768.0),
        other => {
            let _ = ready_tx.send(Err(format!("unsupported sample format: {other:?}")));
            return Captured { samples: Vec::new(), sample_rate };
        }
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("failed to open mic stream: {e}")));
            return Captured { samples: Vec::new(), sample_rate };
        }
    };
    if let Err(e) = stream.play() {
        let _ = ready_tx.send(Err(format!("failed to start mic stream: {e}")));
        return Captured { samples: Vec::new(), sample_rate };
    }

    // Live: unblock voice_start, then record until told to stop.
    let _ = ready_tx.send(Ok(()));
    let _ = stop_rx.recv();
    drop(stream); // stop capturing before we read the buffer out

    let samples = std::mem::take(&mut *buffer.lock().unwrap());
    Captured { samples, sample_rate }
}

// ---------- transcription (subprocess to whisper.cpp) ----------

/// Locations of the prebuilt whisper.cpp CLI and a ggml model. Neither is
/// committed to the repo; both are downloaded once (see the walkthrough).
struct WhisperPaths {
    cli: PathBuf,
    model: PathBuf,
}

/// The install root the prototype looks in by default:
/// `%LOCALAPPDATA%\loomux\whisper\`. `whisper-cli.exe` sits at the root and
/// models under `models\`.
fn whisper_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("loomux").join("whisper"))
}

/// Resolve the whisper CLI and model, in priority order:
///   1. env `LOOMUX_WHISPER_CLI` / `LOOMUX_WHISPER_MODEL` (explicit override);
///   2. the default install dir (`whisper-cli.exe`; model `ggml-base.en.bin`,
///      else `ggml-tiny.en.bin`, else the first `models\*.bin`).
/// Returns a human-readable, actionable error naming where it looked.
fn resolve_whisper() -> Result<WhisperPaths, String> {
    let dir = whisper_dir();

    // --- CLI ---
    let cli = match std::env::var_os("LOOMUX_WHISPER_CLI") {
        Some(p) => PathBuf::from(p),
        None => {
            let d = dir
                .clone()
                .ok_or("cannot resolve %LOCALAPPDATA%")?;
            // Accept either the current name (whisper-cli.exe) or the legacy
            // `main.exe` that older whisper.cpp release zips shipped.
            let candidates = [d.join("whisper-cli.exe"), d.join("main.exe")];
            candidates
                .iter()
                .find(|p| p.is_file())
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "whisper CLI not found. Put whisper-cli.exe in {} or set \
                         LOOMUX_WHISPER_CLI. See doc/demo/voice-prompt.md.",
                        d.display()
                    )
                })?
        }
    };
    if !cli.is_file() {
        return Err(format!("whisper CLI not found at {}", cli.display()));
    }

    // --- model ---
    let model = match std::env::var_os("LOOMUX_WHISPER_MODEL") {
        Some(p) => PathBuf::from(p),
        None => {
            let models = dir
                .ok_or("cannot resolve %LOCALAPPDATA%")?
                .join("models");
            let preferred = ["ggml-base.en.bin", "ggml-tiny.en.bin", "ggml-base.bin"];
            preferred
                .iter()
                .map(|n| models.join(n))
                .find(|p| p.is_file())
                .or_else(|| first_bin_in(&models))
                .ok_or_else(|| {
                    format!(
                        "no Whisper model found in {}. Download e.g. ggml-base.en.bin \
                         or set LOOMUX_WHISPER_MODEL. See doc/demo/voice-prompt.md.",
                        models.display()
                    )
                })?
        }
    };
    if !model.is_file() {
        return Err(format!("Whisper model not found at {}", model.display()));
    }

    Ok(WhisperPaths { cli, model })
}

/// First `*.bin` in `dir` (sorted for determinism), if any.
fn first_bin_in(dir: &std::path::Path) -> Option<PathBuf> {
    let mut bins: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "bin").unwrap_or(false))
        .collect();
    bins.sort();
    bins.into_iter().next()
}

/// Resample, write a scratch WAV, run whisper.cpp, and return the transcript.
fn transcribe(mono16k: &[f32]) -> Result<String, String> {
    let paths = resolve_whisper()?;

    let wav = encode_wav_pcm16(mono16k, WHISPER_SAMPLE_RATE);
    // Scratch WAV in a per-process temp path. std's temp_dir + pid avoids a
    // getrandom-based tempfile name (see the Windows-10 baseline note).
    let wav_path = std::env::temp_dir().join(format!("loomux-voice-{}.wav", std::process::id()));
    std::fs::write(&wav_path, &wav).map_err(|e| format!("write scratch wav: {e}"))?;

    let result = run_whisper(&paths, &wav_path);
    let _ = std::fs::remove_file(&wav_path); // best-effort cleanup
    result
}

/// Invoke whisper.cpp on `wav_path` and parse its stdout into plain text.
fn run_whisper(paths: &WhisperPaths, wav_path: &std::path::Path) -> Result<String, String> {
    use std::process::Command;
    let mut cmd = Command::new(&paths.cli);
    cmd.arg("-m")
        .arg(&paths.model)
        .arg("-f")
        .arg(wav_path)
        .arg("-nt") // no timestamps: stdout is just the recognized text
        .arg("-l")
        .arg("en");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("whisper CLI not found at {}", paths.cli.display())
        } else {
            format!("failed to run whisper: {e}")
        }
    })?;
    if !out.status.success() {
        return Err(format!(
            "whisper failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(parse_whisper_output(&String::from_utf8_lossy(&out.stdout)))
}

// ---------- pure helpers (unit-tested) ----------

/// Linear-interpolation resample of mono `input` from `from` Hz to `to` Hz.
/// Good enough for speech STT; not a polyphase/anti-aliased resampler. Returns
/// the input unchanged when the rates match or it is empty.
pub fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == 0 || to == 0 || from == to {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let out_len = ((input.len() as f64) * (to as f64) / (from as f64)).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let idx = pos.floor() as usize;
        let frac = (pos - idx as f64) as f32;
        let a = input[idx.min(input.len() - 1)];
        let b = input[(idx + 1).min(input.len() - 1)];
        out.push(a + (b - a) * frac);
    }
    out
}

/// Encode mono f32 samples (clamped to [-1, 1]) as a 16-bit PCM WAV byte
/// stream. Minimal 44-byte canonical header — enough for whisper.cpp.
pub fn encode_wav_pcm16(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let bits_per_sample: u16 = 16;
    let channels: u16 = 1;
    let byte_rate = sample_rate * channels as u32 * (bits_per_sample as u32 / 8);
    let block_align = channels * (bits_per_sample / 8);
    let data_len = (samples.len() * 2) as u32;

    let mut buf = Vec::with_capacity(44 + samples.len() * 2);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits_per_sample.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let v = (clamped * i16::MAX as f32).round() as i16;
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf
}

/// Clean whisper.cpp stdout into a single-line prompt string: strip any
/// `[hh:mm:ss.mmm --> ...]` timestamp prefixes, drop whole-line bracket markers
/// like `[BLANK_AUDIO]` / `(speaking)`, and collapse the rest to one space-
/// joined line. Whisper's own logging goes to stderr, so stdout is mostly text.
pub fn parse_whisper_output(stdout: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for raw in stdout.lines() {
        let mut line = raw.trim();
        // Drop a leading "[ 00:00:00.000 --> 00:00:02.000]" timestamp block.
        if line.starts_with('[') {
            if let Some(end) = line.find(']') {
                let inner = &line[1..end];
                // Only strip it if it looks like a timestamp (has "-->" or digits+colons),
                // otherwise it may be a real bracket in speech.
                if inner.contains("-->") {
                    line = line[end + 1..].trim();
                }
            }
        }
        if line.is_empty() {
            continue;
        }
        // Skip non-speech markers that are the entire line, e.g. [BLANK_AUDIO].
        if (line.starts_with('[') && line.ends_with(']'))
            || (line.starts_with('(') && line.ends_with(')'))
        {
            continue;
        }
        parts.push(line.to_string());
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_matching_rate_is_identity() {
        let s = vec![0.0, 0.5, -0.5, 1.0];
        assert_eq!(resample_linear(&s, 16_000, 16_000), s);
    }

    #[test]
    fn resample_empty_is_empty() {
        assert!(resample_linear(&[], 48_000, 16_000).is_empty());
    }

    #[test]
    fn downsample_thirds_length_and_endpoints() {
        // 48k -> 16k is a 3:1 decimation; length divides by ~3 and the first
        // sample is preserved (linear interp at pos 0).
        let input: Vec<f32> = (0..48).map(|i| i as f32 / 48.0).collect();
        let out = resample_linear(&input, 48_000, 16_000);
        assert_eq!(out.len(), 16);
        assert!((out[0] - input[0]).abs() < 1e-6);
    }

    #[test]
    fn upsample_grows_length() {
        let input = vec![0.0, 1.0];
        let out = resample_linear(&input, 8_000, 16_000);
        assert_eq!(out.len(), 4);
        assert!((out[0] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn wav_header_is_well_formed() {
        let wav = encode_wav_pcm16(&[0.0, 1.0, -1.0], 16_000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        // 3 samples * 2 bytes = 6 bytes of PCM data.
        let data_len = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_len, 6);
        assert_eq!(wav.len(), 44 + 6);
        // 16 kHz mono @ 16-bit → byte rate 32000.
        let byte_rate = u32::from_le_bytes([wav[28], wav[29], wav[30], wav[31]]);
        assert_eq!(byte_rate, 32_000);
    }

    #[test]
    fn wav_clamps_and_encodes_full_scale() {
        let wav = encode_wav_pcm16(&[2.0], 16_000); // clamps to 1.0 → i16::MAX
        let sample = i16::from_le_bytes([wav[44], wav[45]]);
        assert_eq!(sample, i16::MAX);
    }

    #[test]
    fn parse_strips_timestamps_and_joins() {
        let out = "[00:00:00.000 --> 00:00:02.000]  Hello there.\n\
                   [00:00:02.000 --> 00:00:04.000]  General Kenobi.\n";
        assert_eq!(parse_whisper_output(out), "Hello there. General Kenobi.");
    }

    #[test]
    fn parse_drops_blank_audio_markers() {
        let out = "  [BLANK_AUDIO]\n  Actual words.\n  (soft music)\n";
        assert_eq!(parse_whisper_output(out), "Actual words.");
    }

    #[test]
    fn parse_plain_no_timestamp_lines() {
        assert_eq!(parse_whisper_output(" just text \n"), "just text");
    }
}
