# Architecture

```
crates/
  dictum-engine/   speech → text; pure Rust + ONNX Runtime; no OS integration
  dictum/          the app: hotkey, mic, pipeline, insertion, tray (Windows today)
```

## Engine (`dictum-engine`)

| Module | Responsibility |
| --- | --- |
| `parakeet.rs` | Parakeet TDT inference: `nemo128.onnx` (log-mel features) → FastConformer encoder (int8) → greedy TDT decoding with the joint `decoder_joint` model. `Interrupt` cancels an in-flight run (ONNX Runtime `RunOptions::terminate` + a flag checked between decoder steps). |
| `boost.rs` | Vocabulary boosting. The user's terms are matched against the model's SentencePiece tokens (case-insensitive, every segmentation); during greedy decoding, tokens that start or continue a term get a logit bonus (12; halved for terms under 5 letters). It is all-or-nothing per term: when a boosted token changes the output, the decoder checkpoints and rolls back (boosting off for that step) unless the term is completed, so a half-forced term never leaves fragments. With a 40-term dev vocabulary the reference transcripts are unchanged (`vocabulary_does_not_change_ordinary_speech`). |
| `vad.rs` | Silero VAD v6 (bundled, 2 MB); 32 ms windows with carried state. |
| `segment.rs` | Turns a recording into segments: hysteresis on VAD probabilities, trimming with padding, `Pause` snapshots and splits at pauses once a segment is ≥ 10 s. Snapshots and segments are identified by `SegmentKey` (exact sample range) so identical audio is never transcribed twice. |
| `resample.rs` | Streaming Kaiser-windowed sinc resampler (polyphase table, exact integer positions) from the device rate to 16 kHz. |
| `vocab.rs` | SentencePiece detokenisation. |
| `text.rs` | Deterministic clean-up: filler removal, voice commands, vocabulary spelling (terms written as listed, split/joined words and one-letter misses in long terms corrected), personal dictionary. No LLM — microseconds, no network. |
| `model.rs` | Pinned model manifest (sizes + SHA-256) and a resumable, verifying downloader. |

Why Parakeet TDT on ONNX Runtime: best accuracy/speed trade-off for local dictation on CPUs
(lower WER than Whisper large-v3 on the Open ASR leaderboard at a fraction of the compute), TDT
decoding skips frames instead of decoding every one, it emits punctuation and casing, and ONNX
Runtime ships prebuilt for Windows (with DirectML), macOS (with CoreML) and Linux.

## App (`dictum`)

```
 keyboard hook ──Command──► controller ──Job──► worker ──Done──► output
 (main thread)              mic → resample      Parakeet         wait for modifiers up,
 hotkey::Machine            → VAD/segmenter     reuse/cancel     paste or type,
                                                speculation      restore clipboard
```

- **`hotkey.rs`** — platform-independent state machine: hold-to-talk, hands-free lock, cancel,
  "this was another shortcut" (another key within 300 ms → abort), resync after missed key-ups.
- **`pipeline.rs`** — `Dictation` (controller side) and `Worker` (engine side). Speculation
  protocol: every pause sends `Speculate{generation}`; any later job that makes it useless bumps
  the generation (`SpecControl::supersede`), which skips it if queued or interrupts it if
  running. `Segment`/`Finish` jobs reuse the session's speculative text when keys match.
- **`audio.rs`** — cpal input stream opened once, then paused/resumed per dictation; follows
  default-device changes; mono mixdown in the callback.
- **`app.rs`** — thread wiring, model download/load, status board → tray, safety nets (missed
  key-up watchdog, max recording length, accidental taps < 250 ms).
- **`update.rs`** — on-demand updates from GitHub Releases (`releases/latest`): the asset is
  checked against GitHub's SHA-256 `digest`, unpacked in memory, written as `*.dictum-new`, then
  swapped in by renaming (allowed for the running exe and loaded DLLs) with the old files kept as
  `*.dictum-old` until the next start; any failed rename rolls the swap back.
  `platform/windows/updates.rs` runs it from the tray with message-box prompts and restarts.
- **`platform/windows/`** — `WH_KEYBOARD_LL` hook (ignores our own injected input via a
  `dwExtraInfo` marker; injects an unassigned key so Win/Alt hotkeys don't open the Start menu),
  `SendInput` paste/typing, clipboard save/restore of every HGLOBAL format plus
  `ExcludeClipboardContentFromMonitorProcessing` / `CanIncludeInClipboardHistory=0`,
  message-only window for cross-thread status updates and clipboard ownership, `tray-icon` menu,
  `PlaySound` cues synthesised at runtime, HKCU `Run` autostart, single-instance mutex, and the
  settings window: a modeless dialog built from an in-memory `DLGTEMPLATEEX` (dialog units, so it
  scales with DPI; Common Controls v6 via the manifest in `build.rs`). Device listing runs off the
  main thread. Saving writes only the changed fields with `toml_edit`, so comments survive, then
  restarts the app. The text conversions behind it (`settings.rs`) are platform independent.
  The dictation indicator is a layered `WS_EX_NOACTIVATE | WS_EX_TRANSPARENT` topmost window on
  the monitor of the foreground window. The board posts it the phase, and the controller feeds
  mic chunks into a shared level meter. A 30 fps timer runs only while it is visible. Frames are
  drawn in software by `indicator.rs` (platform independent, anti-aliased via signed distance
  fields) and handed to `UpdateLayeredWindow`.

## Adding macOS

Implement `platform/macos/` with the same surface as `platform/windows/mod.rs`:

| Function | macOS implementation |
| --- | --- |
| `run` | `NSApplication` run loop + `tray-icon` (`NSStatusItem`) + a `CGEventTap` on `kCGEventKeyDown/KeyUp/FlagsChanged` feeding `hotkey::Machine` (Fn = `kCGEventFlagMaskSecondaryFn`) |
| `insert_text` | `NSPasteboard` save → set (`org.nspasteboard.TransientType` to stay out of clipboard managers) → `CGEventPost` Cmd+V → restore; or `CGEventKeyboardSetUnicodeString` for typing |
| `hotkey_held`, `wait_for_modifiers_released` | `CGEventSourceFlagsState` / `CGEventSourceKeyState` |
| `single_instance`, autostart | lock file / `SMAppService` |
| `Ui::cue` | `NSSound` from the generated WAV bytes |

Permissions: Microphone, Accessibility (event tap + posting events) and Input Monitoring. Then
switch the `cfg(windows)` gates in `main.rs` to `cfg(any(windows, target_os = "macos"))`.
Everything else (engine, pipeline, hotkey machine, config, icons, sounds) is already shared and
tested on macOS in CI.
