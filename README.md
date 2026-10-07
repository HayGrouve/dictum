# Dictum

Hold a hotkey, speak, release — the text appears wherever your cursor is.

Dictum is a fast, private take on [Wispr Flow](https://wisprflow.ai/) with nothing extra: no
window, no account, no cloud, no transcript history. Speech recognition runs entirely on your
machine with NVIDIA's **Parakeet TDT 0.6B v3** (25 languages, including English and Bulgarian,
with punctuation and capitalisation).

## Using it

| Do this | To |
| --- | --- |
| Hold **Ctrl+Win**, speak, release | Dictate into the focused app |
| While holding, tap **Space** | Switch to hands-free; press **Ctrl+Win** again to finish |
| **Esc** while recording | Discard the recording |

A tray icon shows the state (grey = ready, red = listening, amber = transcribing, blue =
loading) and its menu has *Settings…*, *Open log*, *Start with Windows*, *Restart* and *Quit*.
Short sounds mark start/stop (can be turned off).

On first launch Dictum downloads the speech model (~670 MB, checksum-verified, resumable) to
`%LOCALAPPDATA%\Dictum\models`; the tray shows progress.

## Settings

`%APPDATA%\Dictum\config.toml` is created on first run with every option documented. Highlights:

- `hotkey` — e.g. `"ctrl+win"` (default), `"right_ctrl"`, `"right_alt"`, `"f13"`
- `insert_method` — `"paste"` (default; restores your clipboard afterwards and keeps dictated
  text out of clipboard history) or `"type"` (never touches the clipboard)
- `remove_fillers`, `voice_commands` ("new line" / "new paragraph"), `trailing_space`
- `[replacements]` — a personal dictionary, e.g. `"get hub" = "GitHub"`
- `device = "gpu"` — DirectML (any DX12 GPU) for the acoustic model; CPU is the default and is
  already well under real time
- `microphone` — part of a device name; empty follows the Windows default device

Use tray → *Restart* after editing.

## Performance

Measured by `cargo test --release -p dictum-engine --test accuracy` (4 vCPU slice of a
Ryzen 5 5600G under WSL; native Windows on the full CPU is faster):

| | |
| --- | --- |
| Accuracy (LibriSpeech dev-clean sample + JFK) | **2.5 % WER** |
| Speed | **~20× faster than real time** on CPU (11 s of speech → 0.57 s) |
| Text ready after you release the key, typical dictation | **≈ 0 ms** — transcribed speculatively while you pause |
| 150 s continuous dictation | split at pauses and transcribed while you talk; ≈ 0 ms left at release, 3.1 % WER |

How: the model stays loaded and warmed up; the microphone stream stays open but paused (so
starting costs milliseconds and Windows doesn't show the mic as in use); Silero VAD trims silence
and finds pauses; each pause triggers a speculative transcription whose result is reused if you
release without saying more, and cancelled mid-inference if you keep talking. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Privacy

Audio never leaves the machine and is never written to disk. Dictated text is never logged.
When pasting, the text is marked so Windows excludes it from clipboard history and cloud
clipboard, and your previous clipboard contents (all formats) are put back.

## Installing a build

Download `dictum-windows-x64.zip` from the latest CI run, unzip it anywhere and run
`dictum.exe`.

> **Smart App Control / SmartScreen:** builds are not code-signed yet. If Smart App Control is
> on, Windows blocks unsigned apps outright; it can only be allowed by turning Smart App Control
> off (Windows Security → App & browser control) or by signing the binary (e.g. Azure Trusted
> Signing).

Dictum cannot type into apps running as administrator unless it runs as administrator too
(a Windows security boundary).

## Building

Rust 1.88+.

```sh
cargo build --release -p dictum          # Windows: target/release/dictum.exe
cargo test --workspace                   # unit tests (any OS)
DICTUM_MODEL_DIR=~/.cache/dictum cargo test --release -p dictum-engine -- --nocapture
                                         # accuracy/latency on real speech (downloads the model)
cargo run --release -p dictum-engine --example transcribe -- <model-dir> file.wav
```

The engine and all logic (hotkey state machine, pipeline, text clean-up) are platform
independent and tested on Linux, macOS and Windows in CI. The Windows-specific layer (keyboard
hook, paste/typing into a real edit control, clipboard restore) is tested on the Windows runner.

## macOS

The architecture is ready for it: the engine already runs on Apple Silicon (CoreML-capable ONNX
Runtime, tested in CI), and all OS integration sits behind `crates/dictum/src/platform/`. A
macOS backend needs a `CGEventTap` hotkey listener (default hotkey: Fn), `NSPasteboard` +
Cmd+V insertion and an `NSStatusItem`; see the architecture doc.

## Credits

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) — Parakeet (CC BY 4.0), Silero VAD (MIT),
ONNX Runtime (MIT).
