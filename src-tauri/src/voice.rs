//! Voice prompts (issue #58): push-to-talk mic capture → local, open-source
//! Whisper transcription → text handed back to whatever holds focus (the human
//! reviews it and presses Enter; loomux never auto-submits). The routing lives
//! in the frontend (voicecontrol.ts); this module is capture + transcription.
//!
//! ## Platform scope
//!
//! Native capture is **Windows-only** for now. `cpal` pulls
//! `alsa-sys` on Linux (needs `libasound2-dev`) and CoreAudio on macOS, and the
//! transcription path here is Windows-flavoured anyway (`whisper-cli.exe`,
//! `%LOCALAPPDATA%`), so the whole capture + transcription implementation lives
//! under `#[cfg(windows)]`. On other platforms the three `#[tauri::command]`s
//! still exist but return a graceful "not available on this platform" error —
//! the same shape the missing-binary path returns — so the frontend behaves
//! identically everywhere and non-Windows builds don't drag in ALSA/CoreAudio.
//!
//! ## Why this shape (Windows)
//!
//! Two axes had to clear the Windows-10 baseline constraint (no getrandom /
//! ProcessPrng in the shipped binary — see Cargo.toml) *and* keep the repo's
//! `cargo check` gate cheap:
//!
//! * **Capture — `cpal` (native WASAPI).** Vetted getrandom-clean at runtime
//!   (`cargo tree -i getrandom` shows getrandom only under build-dependencies /
//!   Android's oboe-sys, never in cpal's Windows runtime tree). Native capture
//!   also sidesteps the WebView2 `getUserMedia` microphone-permission dance, so
//!   the demo is deterministic. cpal is pure Rust WASAPI bindings — no C++/cmake.
//! * **Transcription — subprocess to a prebuilt whisper.cpp `whisper-cli.exe`**
//!   (git.rs-style `Command`), NOT the `whisper-rs` crate. whisper-rs is itself
//!   getrandom-clean at runtime, but `whisper-rs-sys` builds whisper.cpp from
//!   C++ via cmake, which would drag a cmake + MSVC toolchain requirement into
//!   `cargo check --locked` for everyone. Shelling out keeps the gate a pure-
//!   Rust check and matches how git.rs already integrates an external CLI.
//!
//! The `whisper-cli.exe` binary and the model weights are NOT committed — they
//! discovered at runtime (bundled resources → env vars → %LOCALAPPDATA%; see
//! doc/design/voice.md).
//!
//! The pure helpers ([`resample_linear`], [`encode_wav_pcm16`],
//! [`parse_whisper_output`]) are cross-platform and unit-tested below.

// ---------- Tauri state ----------

/// Managed Tauri state: on Windows it holds at most one active recording; on
/// other platforms it is an empty marker so the command signatures still type.
#[cfg(windows)]
#[derive(Default)]
pub struct VoiceState(std::sync::Mutex<Option<win::Recording>>);

#[cfg(not(windows))]
#[derive(Default)]
pub struct VoiceState;

/// Error surfaced by the voice commands where native capture isn't built in.
#[cfg(not(windows))]
const VOICE_UNAVAILABLE: &str = "voice capture is only available on Windows in this prototype";

// ---------- commands ----------

/// Begin capturing from the default input device.
#[cfg(windows)]
#[tauri::command]
pub fn voice_start(state: tauri::State<'_, VoiceState>) -> Result<(), String> {
    win::start(&state)
}

/// Stop the active recording, transcribe it locally, and return the text.
/// `app` is used to locate the whisper runtime bundled as a Tauri resource.
#[cfg(windows)]
#[tauri::command]
pub fn voice_stop(
    app: tauri::AppHandle,
    state: tauri::State<'_, VoiceState>,
) -> Result<String, String> {
    win::stop(&state, win::bundled_whisper_dir(&app))
}

/// Cancel any active recording without transcribing. Idempotent.
#[cfg(windows)]
#[tauri::command]
pub fn voice_cancel(state: tauri::State<'_, VoiceState>) -> Result<(), String> {
    win::cancel(&state);
    Ok(())
}

#[cfg(not(windows))]
#[tauri::command]
pub fn voice_start(_state: tauri::State<'_, VoiceState>) -> Result<(), String> {
    Err(VOICE_UNAVAILABLE.into())
}

#[cfg(not(windows))]
#[tauri::command]
pub fn voice_stop(
    _app: tauri::AppHandle,
    _state: tauri::State<'_, VoiceState>,
) -> Result<String, String> {
    Err(VOICE_UNAVAILABLE.into())
}

/// No-op off Windows: there is never an in-flight recording to cancel, and the
/// frontend calls this on pane dispose, so it must succeed quietly.
#[cfg(not(windows))]
#[tauri::command]
pub fn voice_cancel(_state: tauri::State<'_, VoiceState>) -> Result<(), String> {
    Ok(())
}

// ---------- Windows implementation ----------

#[cfg(windows)]
mod win {
    use super::{
        duration_secs, encode_wav_pcm16, is_dll_load_failure, parse_whisper_output, resample_linear,
        rms, ChunkedBuffer, VoiceState,
    };
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    /// Sample rate Whisper (ggml) models are trained on. Everything is resampled
    /// to this mono rate before transcription.
    const WHISPER_SAMPLE_RATE: u32 = 16_000;

    /// Max recording length before capture stops growing (the max-duration
    /// guard). Five minutes of dictation is already a very long prompt; past
    /// this we cap and tell the user rather than accumulate unbounded memory.
    const MAX_RECORDING_SECS: u32 = 300;

    /// Audio captured by the recording thread: downmixed mono f32 at the
    /// device's native rate (resampled to 16 kHz only at stop time). `error` is
    /// set if the input stream faulted mid-capture (e.g. the mic was unplugged);
    /// `capped` if the max-duration guard truncated it.
    struct Captured {
        samples: Vec<f32>,
        sample_rate: u32,
        error: Option<String>,
        capped: bool,
    }

    /// The whisper runtime bundled as a Tauri resource: `<resource>/whisper`,
    /// holding `whisper-cli.exe` (+ its DLLs) and `models/`. `None` if the
    /// resource dir can't be resolved (e.g. `cargo test` without a bundle).
    pub fn bundled_whisper_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
        use tauri::Manager;
        app.path().resource_dir().ok().map(|r| r.join("whisper"))
    }

    /// A recording in flight. Sending on (or dropping) `stop_tx` tells the
    /// capture thread to stop; joining yields the captured audio. The cpal
    /// stream itself lives entirely inside that thread because it is `!Send`.
    pub struct Recording {
        stop_tx: Sender<()>,
        join: JoinHandle<Captured>,
    }

    /// Begin capturing. Errors before returning if there is no input device or
    /// the stream can't be built, so the UI can surface a real message.
    pub fn start(state: &VoiceState) -> Result<(), String> {
        let mut slot = state.0.lock().map_err(|_| "voice state poisoned")?;
        if slot.is_some() {
            return Err("already recording".into());
        }

        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        // The thread reports stream-build success/failure so start() can fail
        // synchronously with a useful message.
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let join = std::thread::spawn(move || capture_loop(stop_rx, ready_tx));

        match ready_rx.recv() {
            Ok(Ok(())) => {
                *slot = Some(Recording { stop_tx, join });
                Ok(())
            }
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

    /// Stop and transcribe. Empty/near-silent captures return an empty string
    /// rather than an error, so a mis-click just yields nothing to insert — but
    /// an empty capture caused by a mid-record device fault surfaces that error.
    /// `bundled` is the resource-dir whisper runtime (highest resolution priority).
    pub fn stop(state: &VoiceState, bundled: Option<PathBuf>) -> Result<String, String> {
        let recording = {
            let mut slot = state.0.lock().map_err(|_| "voice state poisoned")?;
            slot.take().ok_or("not recording")?
        };
        let _ = recording.stop_tx.send(());
        let captured = recording
            .join
            .join()
            .map_err(|_| "recording thread panicked".to_string())?;

        // Diagnostics: duration + RMS of the raw capture. Always cheap; logged
        // when LOOMUX_VOICE_KEEP_WAV is set so a bad live capture is inspectable.
        if keep_wav_enabled() {
            eprintln!(
                "voice: captured {} samples @ {} Hz ({:.1}s), rms={:.5}, capped={}",
                captured.samples.len(),
                captured.sample_rate,
                duration_secs(captured.samples.len(), captured.sample_rate),
                rms(&captured.samples),
                captured.capped,
            );
        }

        if captured.samples.is_empty() {
            // Distinguish "you didn't say anything" from "the mic died".
            return match captured.error {
                Some(e) => Err(format!("microphone stopped mid-recording: {e}")),
                None => Ok(String::new()),
            };
        }
        // We have audio; transcribe it even if a late device error also occurred
        // (partial speech beats dropping it).
        let mono16k = resample_linear(&captured.samples, captured.sample_rate, WHISPER_SAMPLE_RATE);
        let text = transcribe(&mono16k, bundled)?;
        // Surface the max-duration cap once we have a (partial) transcript, so
        // the user knows why a very long dictation was cut off.
        if captured.capped {
            return Ok(format!("{text} [voice: recording capped at {MAX_RECORDING_SECS}s]"));
        }
        Ok(text)
    }

    /// Debug switch: LOOMUX_VOICE_KEEP_WAV=1 preserves the scratch WAV and logs
    /// capture diagnostics — the tool we lacked while chasing the long-recording
    /// bug (#58). Any non-empty, non-"0"/"false" value enables it.
    fn keep_wav_enabled() -> bool {
        match std::env::var("LOOMUX_VOICE_KEEP_WAV") {
            Ok(v) => !matches!(v.trim(), "" | "0" | "false" | "False" | "FALSE"),
            Err(_) => false,
        }
    }

    /// Cancel any active recording without transcribing.
    pub fn cancel(state: &VoiceState) {
        let recording = state.0.lock().ok().and_then(|mut slot| slot.take());
        if let Some(r) = recording {
            let _ = r.stop_tx.send(());
            let _ = r.join.join();
        }
    }

    /// Body of the capture thread. Builds the input stream, reports readiness,
    /// then blocks until stopped, appending downmixed-mono f32 samples to the
    /// buffer it returns. The cpal `Stream` never leaves this thread (`!Send`).
    fn capture_loop(stop_rx: Receiver<()>, ready_tx: Sender<Result<(), String>>) -> Captured {
        let host = cpal::default_host();
        let device = match host.default_input_device() {
            Some(d) => d,
            None => {
                let _ = ready_tx.send(Err("no microphone / input device found".into()));
                return Captured { samples: Vec::new(), sample_rate: WHISPER_SAMPLE_RATE, error: None, capped: false };
            }
        };
        let supported = match device.default_input_config() {
            Ok(c) => c,
            Err(e) => {
                let _ = ready_tx.send(Err(format!("no default input config: {e}")));
                return Captured { samples: Vec::new(), sample_rate: WHISPER_SAMPLE_RATE, error: None, capped: false };
            }
        };

        let sample_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        // Chunked block buffer — NEVER reallocs a filled block, so the audio
        // callback has bounded cost (see ChunkedBuffer). The cap is the max-
        // duration guard, in samples at the device rate.
        let cap = (MAX_RECORDING_SECS as u64 * sample_rate.max(1) as u64) as usize;
        let buffer = Arc::new(Mutex::new(ChunkedBuffer::new(cap)));
        // Shared slot the stream error callback writes to (mic unplugged / device
        // lost mid-record). stop() reads it to distinguish silence from a fault.
        let err_slot = Arc::new(Mutex::new(None::<String>));

        // One closure per sample format: downmix each frame to a single mono
        // sample (average of channels) and append. Whisper is mono anyway. The
        // lock is uncontended during capture (stop() only locks after the stream
        // is dropped), and ChunkedBuffer::push does no reallocating memcpy — so
        // this stays within the callback's real-time budget. The error closure is
        // rebuilt per arm (it captures a fresh Arc clone) so it needn't be Copy.
        macro_rules! build {
            ($t:ty, $to_f32:expr) => {{
                let buf = buffer.clone();
                let errs = err_slot.clone();
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
                    move |e| {
                        eprintln!("voice: input stream error: {e}");
                        *errs.lock().unwrap() = Some(e.to_string());
                    },
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
                return Captured { samples: Vec::new(), sample_rate, error: None, capped: false };
            }
        };
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                // A build failure here is often the OS denying mic access.
                let _ = ready_tx.send(Err(format!(
                    "couldn't open the microphone: {e} (check Windows microphone privacy settings)"
                )));
                return Captured { samples: Vec::new(), sample_rate, error: None, capped: false };
            }
        };
        if let Err(e) = stream.play() {
            let _ = ready_tx.send(Err(format!("failed to start mic stream: {e}")));
            return Captured { samples: Vec::new(), sample_rate, error: None, capped: false };
        }

        // Live: unblock start(), then record until told to stop.
        let _ = ready_tx.send(Ok(()));
        let _ = stop_rx.recv();
        drop(stream); // stop capturing (and the callbacks) before we drain

        // Now that the stream is stopped nothing else touches the buffer, so this
        // take + flatten (the one large allocation) runs off the audio thread.
        let taken = std::mem::replace(&mut *buffer.lock().unwrap(), ChunkedBuffer::new(0));
        let capped = taken.is_capped();
        let samples = taken.into_samples();
        let error = err_slot.lock().unwrap().take();
        Captured { samples, sample_rate, error, capped }
    }

    // ----- transcription (subprocess to whisper.cpp) -----

    /// Locations of the prebuilt whisper.cpp CLI and a ggml model. Neither is
    /// committed to the repo; both are downloaded once (see the walkthrough).
    struct WhisperPaths {
        cli: PathBuf,
        model: PathBuf,
    }

    /// The power-user install root: `%LOCALAPPDATA%\loomux\whisper\`.
    fn local_whisper_dir() -> Option<PathBuf> {
        dirs::data_local_dir().map(|d| d.join("loomux").join("whisper"))
    }

    /// CLI names to accept in a whisper dir: the current one, then the legacy
    /// `main.exe` older whisper.cpp release zips shipped.
    const CLI_NAMES: [&str; 2] = ["whisper-cli.exe", "main.exe"];

    /// Resolve the whisper CLI and model. Resolution order (per the shipped
    /// design): **bundled resources → `LOOMUX_WHISPER_*` env → %LOCALAPPDATA%**.
    /// `bundled` is `<resource>/whisper`. Returns an actionable error naming
    /// every place it looked.
    fn resolve_whisper(bundled: &Option<PathBuf>) -> Result<WhisperPaths, String> {
        Ok(WhisperPaths {
            cli: resolve_cli(bundled)?,
            model: resolve_model(bundled)?,
        })
    }

    /// whisper-cli.exe: bundled dir → `LOOMUX_WHISPER_CLI` → %LOCALAPPDATA%.
    fn resolve_cli(bundled: &Option<PathBuf>) -> Result<PathBuf, String> {
        if let Some(b) = bundled {
            if let Some(p) = CLI_NAMES.iter().map(|n| b.join(n)).find(|p| p.is_file()) {
                return Ok(p);
            }
        }
        if let Some(p) = std::env::var_os("LOOMUX_WHISPER_CLI") {
            let p = PathBuf::from(p);
            return if p.is_file() {
                Ok(p)
            } else {
                Err(format!("LOOMUX_WHISPER_CLI is set but not a file: {}", p.display()))
            };
        }
        let d = local_whisper_dir().ok_or("cannot resolve %LOCALAPPDATA%")?;
        CLI_NAMES
            .iter()
            .map(|n| d.join(n))
            .find(|p| p.is_file())
            .ok_or_else(|| {
                format!(
                    "whisper CLI not found (looked in bundled resources, \
                     LOOMUX_WHISPER_CLI, and {}). See doc/design/voice.md.",
                    d.display()
                )
            })
    }

    /// Model: bundled `models/` → `LOOMUX_WHISPER_MODEL` → %LOCALAPPDATA%\models.
    fn resolve_model(bundled: &Option<PathBuf>) -> Result<PathBuf, String> {
        if let Some(p) = bundled.as_ref().and_then(|b| pick_model(&b.join("models"))) {
            return Ok(p);
        }
        if let Some(p) = std::env::var_os("LOOMUX_WHISPER_MODEL") {
            let p = PathBuf::from(p);
            return if p.is_file() {
                Ok(p)
            } else {
                Err(format!("LOOMUX_WHISPER_MODEL is set but not a file: {}", p.display()))
            };
        }
        let models = local_whisper_dir()
            .ok_or("cannot resolve %LOCALAPPDATA%")?
            .join("models");
        pick_model(&models).ok_or_else(|| {
            format!(
                "no Whisper model found (looked in bundled resources, \
                 LOOMUX_WHISPER_MODEL, and {}). See doc/design/voice.md.",
                models.display()
            )
        })
    }

    /// Preferred model in `dir` (base.en, then tiny.en, then base), else the
    /// first `*.bin` present.
    fn pick_model(dir: &Path) -> Option<PathBuf> {
        for n in ["ggml-base.en.bin", "ggml-tiny.en.bin", "ggml-base.bin"] {
            let p = dir.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
        first_bin_in(dir)
    }

    /// First `*.bin` in `dir` (sorted for determinism), if any.
    fn first_bin_in(dir: &Path) -> Option<PathBuf> {
        let mut bins: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "bin").unwrap_or(false))
            .collect();
        bins.sort();
        bins.into_iter().next()
    }

    /// Write a scratch WAV, run whisper.cpp, and return the transcript.
    fn transcribe(mono16k: &[f32], bundled: Option<PathBuf>) -> Result<String, String> {
        let paths = resolve_whisper(&bundled)?;

        let wav = encode_wav_pcm16(mono16k, WHISPER_SAMPLE_RATE);
        // Scratch WAV in a per-process temp path. std's temp_dir + pid avoids a
        // getrandom-based tempfile name (see the Windows-10 baseline note).
        let wav_path =
            std::env::temp_dir().join(format!("loomux-voice-{}.wav", std::process::id()));
        std::fs::write(&wav_path, &wav).map_err(|e| format!("write scratch wav: {e}"))?;

        let keep = keep_wav_enabled();
        if keep {
            eprintln!(
                "voice: wrote {} ({:.1}s @ {} Hz, rms={:.5}) — kept for inspection",
                wav_path.display(),
                duration_secs(mono16k.len(), WHISPER_SAMPLE_RATE),
                WHISPER_SAMPLE_RATE,
                rms(mono16k),
            );
        }

        let result = run_whisper(&paths, &wav_path);
        if !keep {
            let _ = std::fs::remove_file(&wav_path); // best-effort cleanup
        }
        result
    }

    /// Invoke whisper.cpp on `wav_path` and parse its stdout into plain text.
    fn run_whisper(paths: &WhisperPaths, wav_path: &Path) -> Result<String, String> {
        use std::os::windows::process::CommandExt;
        use std::process::Command;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let out = Command::new(&paths.cli)
            .arg("-m")
            .arg(&paths.model)
            .arg("-f")
            .arg(wav_path)
            .arg("-nt") // no timestamps: stdout is just the recognized text
            .arg("-l")
            .arg("en")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| {
                // ERROR_MOD_NOT_FOUND (126) at CreateProcess = the exe's imported
                // DLLs are missing (the demo failure). 127 ≈ not found.
                match e.raw_os_error() {
                    Some(126) | Some(127) => dll_error(&paths.cli),
                    _ if e.kind() == std::io::ErrorKind::NotFound => {
                        format!("whisper CLI not found at {}", paths.cli.display())
                    }
                    _ => format!("failed to run whisper: {e}"),
                }
            })?;
        if !out.status.success() {
            // The process started but exited with a DLL-load NTSTATUS → same fix.
            if let Some(code) = out.status.code() {
                if is_dll_load_failure(code) {
                    return Err(dll_error(&paths.cli));
                }
            }
            return Err(format!(
                "whisper failed (exit {:?}): {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(parse_whisper_output(&String::from_utf8_lossy(&out.stdout)))
    }

    /// The actionable "missing DLLs" message (the human hit this in the demo:
    /// whisper-cli.exe copied without its whisper.cpp / ggml DLLs).
    fn dll_error(cli: &Path) -> String {
        format!(
            "whisper-cli.exe is missing its DLLs — copy the .dll files from the \
             whisper.cpp release next to {}. See doc/design/voice.md.",
            cli.display()
        )
    }
}

// ---------- pure helpers (cross-platform, unit-tested) ----------

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

/// Does a process exit code mean the image couldn't load its dependent DLLs?
/// This is the failure the human hit in the demo: whisper-cli.exe present but
/// its whisper.cpp / ggml DLLs weren't copied next to it. The NTSTATUS values
/// surface as the process exit code on Windows:
///   0xC0000135 STATUS_DLL_NOT_FOUND, 0xC000007B STATUS_INVALID_IMAGE_FORMAT,
///   0xC0000139 STATUS_ENTRYPOINT_NOT_FOUND; 127 is the shell "not found" code.
/// Pure + cross-platform so it can be unit-tested off Windows.
pub fn is_dll_load_failure(code: i32) -> bool {
    matches!(code as u32, 0xC000_0135 | 0xC000_007B | 0xC000_0139 | 127)
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
                // Only strip it if it looks like a timestamp (has "-->"),
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

/// Accumulates captured mono f32 samples in fixed-size blocks. This is the fix
/// for the long-recording bug (#58): the old capture appended into one growing
/// `Vec<f32>` from inside the WASAPI real-time callback, so on long recordings
/// the Vec's doubling reallocs copied multi-MB *inside the audio callback* —
/// blowing its hard deadline, starving/glitching the stream, and yielding audio
/// whisper heard as silence. Blocks never realloc (each is filled to its
/// preallocated capacity, then a fresh fixed-size block is started), so the
/// per-callback cost is bounded: O(samples) plus at most one small constant
/// allocation per block boundary. The single flat `Vec` is materialized once,
/// off the audio thread, at stop time. Pure + unit-tested.
pub struct ChunkedBuffer {
    blocks: Vec<Vec<f32>>,
    current: Vec<f32>,
    len: usize,
    /// Hard cap (samples) — the max-duration guard. Samples past it are dropped
    /// and `capped` latches, rather than growing memory without bound.
    cap: usize,
    capped: bool,
}

impl ChunkedBuffer {
    /// Samples per block: ~0.34 s at 48 kHz. Big enough that block-boundary
    /// allocations are rare, small enough (64 KiB) that one is cheap.
    pub const BLOCK: usize = 16_384;

    /// New buffer that accepts up to `cap` samples before capping.
    pub fn new(cap: usize) -> Self {
        Self {
            blocks: Vec::new(),
            current: Vec::with_capacity(Self::BLOCK),
            len: 0,
            cap,
            capped: false,
        }
    }

    /// Append one sample. Rolls to a fresh block on the boundary; drops (and
    /// latches `capped`) once the cap is reached. No realloc of a filled block.
    #[inline]
    pub fn push(&mut self, sample: f32) {
        if self.len >= self.cap {
            self.capped = true;
            return;
        }
        if self.current.len() == Self::BLOCK {
            let full = std::mem::replace(&mut self.current, Vec::with_capacity(Self::BLOCK));
            self.blocks.push(full);
        }
        self.current.push(sample);
        self.len += 1;
    }

    /// Total samples accumulated.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True if the cap was hit and later samples were dropped.
    pub fn is_capped(&self) -> bool {
        self.capped
    }

    /// Flatten to a single contiguous `Vec` (one allocation, done off the audio
    /// thread at stop time). Consumes self.
    pub fn into_samples(self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.len);
        for b in &self.blocks {
            out.extend_from_slice(b);
        }
        out.extend_from_slice(&self.current);
        out
    }
}

/// Root-mean-square amplitude of `samples` (0.0 for empty). Used by the
/// LOOMUX_VOICE_KEEP_WAV diagnostic to log how "loud" a capture actually was —
/// a near-zero RMS on a long recording is the fingerprint of a silent capture.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}

/// Duration in seconds of `sample_count` mono samples at `sample_rate` Hz.
pub fn duration_secs(sample_count: usize, sample_rate: u32) -> f32 {
    if sample_rate == 0 {
        return 0.0;
    }
    sample_count as f32 / sample_rate as f32
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

    #[test]
    fn dll_load_failure_codes_are_recognized() {
        // STATUS_DLL_NOT_FOUND / INVALID_IMAGE_FORMAT / ENTRYPOINT_NOT_FOUND
        // arrive as sign-extended i32 exit codes; 127 is the shell form.
        assert!(is_dll_load_failure(0xC000_0135u32 as i32));
        assert!(is_dll_load_failure(0xC000_007Bu32 as i32));
        assert!(is_dll_load_failure(0xC000_0139u32 as i32));
        assert!(is_dll_load_failure(127));
    }

    #[test]
    fn normal_exit_codes_are_not_dll_failures() {
        assert!(!is_dll_load_failure(0)); // success
        assert!(!is_dll_load_failure(1)); // ordinary error
        assert!(!is_dll_load_failure(2));
    }

    #[test]
    fn chunked_buffer_accumulates_in_order_across_blocks() {
        // Span several blocks (BLOCK = 16384) to exercise the roll-over path.
        let n = ChunkedBuffer::BLOCK * 2 + 500;
        let mut buf = ChunkedBuffer::new(usize::MAX);
        for i in 0..n {
            buf.push(i as f32);
        }
        assert_eq!(buf.len(), n);
        assert!(!buf.is_capped());
        let out = buf.into_samples();
        assert_eq!(out.len(), n);
        // Exact order preserved across block boundaries.
        assert_eq!(out[0], 0.0);
        assert_eq!(out[ChunkedBuffer::BLOCK], ChunkedBuffer::BLOCK as f32);
        assert_eq!(out[n - 1], (n - 1) as f32);
    }

    #[test]
    fn chunked_buffer_caps_and_latches() {
        let cap = ChunkedBuffer::BLOCK + 10;
        let mut buf = ChunkedBuffer::new(cap);
        for i in 0..(cap + 5000) {
            buf.push(i as f32);
        }
        assert!(buf.is_capped());
        assert_eq!(buf.len(), cap); // never exceeds the cap
        assert_eq!(buf.into_samples().len(), cap);
    }

    #[test]
    fn chunked_buffer_empty() {
        let buf = ChunkedBuffer::new(100);
        assert!(buf.is_empty());
        assert!(!buf.is_capped());
        assert!(buf.into_samples().is_empty());
    }

    #[test]
    fn rms_of_full_scale_square_is_one() {
        assert!((rms(&[1.0, -1.0, 1.0, -1.0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rms_of_silence_and_empty_is_zero() {
        assert_eq!(rms(&[0.0, 0.0, 0.0]), 0.0);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn duration_secs_basic() {
        assert!((duration_secs(16_000, 16_000) - 1.0).abs() < 1e-6);
        assert!((duration_secs(48_000, 16_000) - 3.0).abs() < 1e-6);
        assert_eq!(duration_secs(1_000, 0), 0.0); // guard against div-by-zero
    }
}
