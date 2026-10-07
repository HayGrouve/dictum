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

While you dictate, a small bar at the bottom of the screen shows that Dictum is listening, with
live levels from your microphone. In hands-free mode its red dot becomes a stop square, and it
turns amber if transcription takes a moment. It never takes focus and clicks pass through it.
Turn it off with `indicator = false` or in *Settings…*.

A tray icon shows the state (grey = ready, red = listening, amber = transcribing, blue =
loading) and its menu has *Settings…*, *Open log*, *Start with Windows*, *Check for updates*,
*Restart* and *Quit*.
Short sounds mark start/stop (can be turned off).

On first launch Dictum downloads the speech model (~670 MB, checksum-verified, resumable) to
`%LOCALAPPDATA%\Dictum\models`; the tray shows progress.

## Settings

Tray → *Settings…* opens a window with the settings you're likely to change: hotkey, microphone,
the on-screen indicator, sounds, *Start with Windows*, how text is inserted, filler-word removal, voice commands, trailing
space, vocabulary and replacements. *Save* writes them to the config file and restarts Dictum to
apply them.

Everything is stored in `%APPDATA%\Dictum\config.toml`, created on first run with every option
documented (the settings window keeps your comments and formatting). Highlights:

- `hotkey` — e.g. `"ctrl+win"` (default), `"right_ctrl"`, `"right_alt"`, `"f13"`
- `insert_method` — `"paste"` (default; restores your clipboard afterwards and keeps dictated
  text out of clipboard history) or `"type"` (never touches the clipboard)
- `remove_fillers`, `voice_commands` ("new line" / "new paragraph"), `trailing_space`
- `vocabulary` — terms speech recognition tends to get wrong, written the way you want them, e.g.
  `["Claude Code", "Vercel", "shadcn", "TanStack", "Convex", "pnpm"]`. Recognition favours them
  when the audio is ambiguous (inside the decoder, not by guessing afterwards), and near misses
  are corrected: "Shadn" → shadcn, "turbo repo" → Turborepo, "next.js" → Next.js. A long list
  doesn't change ordinary speech; see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- `[replacements]` — a personal dictionary for anything `vocabulary` doesn't catch, e.g.
  `"versal" = "Vercel"`
- `device = "gpu"` — *experimental*: DirectML (any DX12 GPU) for the acoustic model. CPU is the
  default and already far faster than real time; the GPU path is untested on real hardware
- `microphone` — part of a device name; empty follows the Windows default device

After editing the file by hand, use tray → *Restart*.

## Performance

Measured by `cargo test --release -p dictum-engine --test accuracy` (same results in CI on
Windows, macOS and Linux):

| | |
| --- | --- |
| Accuracy (LibriSpeech dev-clean sample + JFK) | **2.5 % WER** |
| Speed (4 vCPU slice of a Ryzen 5 5600G, WSL) | **~19× faster than real time** on CPU (11 s of speech → 0.57 s) |
| Speed (GitHub's Windows runner, only 2 vCPUs) | ~6× faster than real time (11 s → 1.9 s) |
| Text ready after you release the key, typical dictation | **≈ 0 ms** (measured 12–615 µs) — transcribed speculatively while you pause |
| 150 s continuous dictation | split at pauses and transcribed while you talk; ≈ 0 ms left at release, 3.1 % WER |

Inference speed scales with physical cores (ONNX Runtime uses one thread per core by default;
override with `threads`). Speculation makes it matter little for typical dictation: the text is
usually ready before you let go of the key.

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

Download `dictum-windows-x64.zip` from the latest release (or the latest CI run's artifacts),
unzip it anywhere and run `dictum.exe`. The zip contains the exe plus the DLLs it needs (MSVC
runtime, DirectML); nothing has to be installed.

## Updating

Tray → *Check for updates* asks GitHub for the latest release; Dictum never checks on its own.
If there is a newer version it shows the release notes and asks first. The zip is verified
against the SHA-256 checksum GitHub publishes, the new files are swapped in next to the old ones
(rolled back if anything fails) and Dictum restarts. Settings, vocabulary and the speech model
live elsewhere and are kept.

## Releasing

1. Bump `version` in the root `Cargo.toml` (e.g. `0.3.0`) in a PR and merge it.
2. Tag the merge commit: `git tag v0.3.0 && git push origin v0.3.0`.
3. CI builds and tests everything, refuses to publish if the tag doesn't match `Cargo.toml`,
   and creates the GitHub release with `dictum-windows-x64.zip` and generated notes.
   *Check for updates* sees it from then on.

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

## License

Dictum is [MIT](LICENSE) licensed. Bundled and downloaded third-party components keep their own
licenses (see above).
