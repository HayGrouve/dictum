# Dictum

Hold a hotkey, speak, release — the text appears wherever your cursor is.

Dictum is a fast, private take on [Wispr Flow](https://wisprflow.ai/) with nothing extra: no
window, no account, no cloud, no transcript history. It runs on Windows and on Apple Silicon Macs.
An optional second hotkey has Claude polish what you said into a clean prompt; only that one
sends anything off the machine.
Speech recognition runs entirely on your machine with NVIDIA's **Parakeet TDT 0.6B v3** (25
languages, including English and Bulgarian, with punctuation and capitalisation).

## Using it

| Do this | To |
| --- | --- |
| Hold the hotkey, speak, release | Dictate into the focused app |
| While holding, tap **Space** | Switch to hands-free; press the hotkey again to finish |
| **Esc** while recording | Discard the recording |

The hotkey is **Ctrl+Win** on Windows and **Fn** (🌐) on macOS; *Settings…* changes it.
An optional second hotkey dictates polished prompts (see below).

## Polished prompts (optional)

A second hotkey sends what you said to Claude, which rewrites it into the prompt you meant to
type: self-corrections applied ("make it blue, actually scratch that, green" → "Make it green."),
sentences repaired, misheard technical terms fixed, and longer requests structured as a goal plus
bullet points with code names in backticks. It is tuned for prompts to AI coding assistants. The
normal hotkey stays exactly as it is, so you can switch between the two.

It is off until you set it up in *Settings…* → *Polished prompts*: pick a hotkey ("Off" turns
it off again) and paste your API key from console.anthropic.com. The key field then shows the
first half of the key and stars for the rest; clear it to remove the key. The same settings, and
a few more, are in `config.toml`:

```toml
[polish]
hotkey = "right_option"        # or "fn+shift", "right_cmd", "right_ctrl", …
api_key = "sk-ant-…"           # from console.anthropic.com; or set ANTHROPIC_API_KEY
model = "claude-haiku-5-5"
timeout_ms = 4000              # after this, the plain text is inserted instead
instructions = "We use Next.js App Router, Tailwind and Convex."   # optional
```

Rewrites start while you are still talking: at every pause the text so far is sent, so when you
release the key the result is often on its way already. The indicator stays amber until it
arrives. If it fails or takes longer than `timeout_ms`, the plain text is inserted with an error
sound. Your `vocabulary` is passed along so its spellings survive. A longer hotkey that contains
the normal one (`fn+shift` with `fn`) works if you press the extra key within 300 ms.

If polishing fails, the log (*Open log*) says why. Common causes: the key's ID (`apikey_…`)
pasted instead of the key, a key that isn't scoped to a workspace (create it inside one), or no
API credits on the account (API usage is billed separately from a Claude subscription).

While you dictate, a small bar at the bottom of the screen shows that Dictum is listening, with
live levels from your microphone. In hands-free mode its red dot becomes a stop square, and it
turns amber if transcription takes a moment. It never takes focus and clicks pass through it.
Turn it off with `indicator = false` or in *Settings…*.

An icon in the tray (Windows) or menu bar (macOS) shows the state (grey = ready, red =
listening, amber = transcribing, blue = loading) and its menu has *Settings…*, *Open log*,
*Start with Windows* / *Start at login*, *Check for updates*, *Restart* and *Quit*.
Short sounds mark start/stop (can be turned off).

On first launch Dictum downloads the speech model (~670 MB, checksum-verified, resumable) to
`%LOCALAPPDATA%\Dictum\models` on Windows or `~/Library/Application Support/Dictum/models` on
macOS; the icon shows progress.

## Settings

*Settings…* opens a window with the settings you're likely to change: hotkey, microphone, the
on-screen indicator, sounds, starting with the computer, how text is inserted, filler-word and
stutter removal, voice commands, trailing space, the hotkey and API key for polished prompts,
vocabulary and replacements, plus a reminder of the controls above. *Save* writes them to the config file and restarts Dictum to apply them.

Everything is stored in `config.toml` (`%APPDATA%\Dictum` on Windows,
`~/Library/Application Support/Dictum` on macOS), created on first run with every option
documented (the settings window keeps your comments and formatting). Highlights:

- `hotkey` — e.g. `"ctrl+win"` (Windows default), `"fn"` (macOS default), `"right_ctrl"`,
  `"right_alt"` / `"right_option"`, `"right_cmd"`, `"f13"`
- `insert_method` — `"paste"` (default; restores your clipboard afterwards and keeps dictated
  text out of clipboard history) or `"type"` (never touches the clipboard)
- `remove_fillers`, `voice_commands` ("new line" / "new paragraph"), `trailing_space`
- `remove_stutters` — "I I I want to" → "I want to", "w- want" → "want"; words people double on
  purpose ("had had", "very very") are kept
- `hands_free_key`, `cancel_key` — `"space"` and `"escape"` by default
- `vocabulary` — terms speech recognition tends to get wrong, written the way you want them, e.g.
  `["Claude Code", "Vercel", "shadcn", "TanStack", "Convex", "pnpm"]`. Recognition favours them
  when the audio is ambiguous (inside the decoder, not by guessing afterwards), and near misses
  are corrected: "Shadn" → shadcn, "turbo repo" → Turborepo, "next.js" → Next.js. A long list
  doesn't change ordinary speech; see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- `[replacements]` — a personal dictionary for anything `vocabulary` doesn't catch, e.g.
  `"versal" = "Vercel"`
- `device = "gpu"` — *experimental*: DirectML (any DX12 GPU) on Windows, CoreML on macOS, for the
  acoustic model. CPU is the default and already far faster than real time; the GPU path is
  untested on real hardware
- `microphone` — part of a device name; empty follows the system default device

After editing the file by hand, use *Restart* in the menu.

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
The one exception is the optional polish hotkey: its dictations' text (never audio) is sent to
Anthropic's API, along with your `vocabulary` and `instructions`. Without `[polish]` set up,
nothing is sent anywhere.
When pasting, your previous clipboard contents (all formats) are put back and the dictated text
stays out of clipboard history: Windows is told to exclude it from clipboard history and cloud
clipboard, and on macOS it is marked transient so clipboard managers skip it.

## Installing

### Windows

Download `dictum-windows-x64.zip` from the latest release (or the latest CI run's artifacts),
unzip it anywhere and run `dictum.exe`. The zip contains the exe plus the DLLs it needs (MSVC
runtime, DirectML); nothing has to be installed.

> **Smart App Control / SmartScreen:** builds are not code-signed yet. If Smart App Control is
> on, Windows blocks unsigned apps outright; it can only be allowed by turning Smart App Control
> off (Windows Security → App & browser control) or by signing the binary (e.g. Azure Trusted
> Signing).

Dictum cannot type into apps running as administrator unless it runs as administrator too
(a Windows security boundary).

### macOS (Apple Silicon)

1. Download `dictum-macos-arm64.zip` from the latest release, unzip it and drag **Dictum.app**
   into **Applications**.
2. Open it. Dictum is not notarized by Apple, so macOS blocks it the first time: in System
   Settings → Privacy & Security, click **Open Anyway** next to the note about Dictum.
3. Allow **Microphone**, **Accessibility** and **Input Monitoring** for Dictum, all in System
   Settings → Privacy & Security. On macOS 27, Accessibility is called **Device Control and Data
   Access**. Until both keyboard permissions are on, the menu bar icon stays red and its menu says
   what is missing.
4. In System Settings → Keyboard, set **Press 🌐 key to** to **Do Nothing**, so releasing Fn
   doesn't open the emoji picker or Apple's dictation. Or pick another hotkey in *Settings…*.

While a password field or Terminal's *Secure Keyboard Entry* is active, macOS hides keystrokes
from every app, so the hotkey doesn't work there.

## Updating

*Check for updates* in the menu asks GitHub for the latest release; Dictum never checks on its
own. If there is a newer version it shows the release notes and asks first. The zip is verified
against the SHA-256 checksum GitHub publishes, the new version is swapped in next to the old one
(rolled back if anything fails) and Dictum restarts. On Windows that replaces the exe and its
DLLs; on macOS the whole Dictum.app. Settings, vocabulary and the speech model live elsewhere and
are kept. Every macOS release is signed with the same key, so the permissions you granted stay.

## Releasing

1. Bump `version` in the root `Cargo.toml` (e.g. `0.5.0`) in a PR and merge it.
2. Tag the merge commit: `git tag v0.5.0 && git push origin v0.5.0`.
3. CI builds and tests everything, refuses to publish if the tag doesn't match `Cargo.toml`,
   and creates the GitHub release with `dictum-windows-x64.zip`, `dictum-macos-arm64.zip` and
   generated notes. *Check for updates* sees it from then on.

The macOS app is signed with the self-signed identity "Dictum Signing", which CI reads from the
`MACOS_SIGNING_P12` and `MACOS_SIGNING_PASSWORD` secrets (`scripts/macos-signing-cert.sh
--github` created them). macOS ties permissions to that identity, so keep it: a new one makes
everyone grant the permissions again. Tag builds fail without it.

## Building

Rust 1.88+.

```sh
cargo build --release -p dictum          # Windows: target/release/dictum.exe
scripts/macos-bundle.sh --install        # macOS: target/release/Dictum.app, copied to /Applications
cargo test --workspace                   # unit tests (any OS)
DICTUM_MODEL_DIR=~/.cache/dictum cargo test --release -p dictum-engine -- --nocapture
                                         # accuracy/latency on real speech (downloads the model)
cargo run --release -p dictum-engine --example transcribe -- <model-dir> file.wav
ANTHROPIC_API_KEY=sk-ant-… cargo test -p dictum live_rewrites -- --ignored --nocapture
                                         # sample dictations through the polish rewrite
```

On macOS, `scripts/macos-bundle.sh` signs with "Dictum Signing" when it is in your keychain, so
your builds keep their permissions across rebuilds; without it they are signed ad hoc and need
the permissions granted again after each build. `scripts/macos-signing-cert.sh` creates a new
identity, which only makes sense for a fork with its own releases.

The engine and all logic (hotkey state machine, pipeline, text clean-up, the settings form) are
platform independent and tested on Linux, macOS and Windows in CI. The Windows-specific layer
(keyboard hook, paste/typing into a real edit control, clipboard restore, the settings dialog) is
tested on the Windows runner. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how the macOS
layer is built.

## Credits

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) — Parakeet (CC BY 4.0), Silero VAD (MIT),
ONNX Runtime (MIT).

## License

Dictum is [MIT](LICENSE) licensed. Bundled and downloaded third-party components keep their own
licenses (see above).
