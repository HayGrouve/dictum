# Third-party notices

## Speech recognition model (downloaded on first run)

**NVIDIA Parakeet TDT 0.6B v3** — © NVIDIA, licensed under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
Model card: <https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3>.
Dictum downloads the ONNX export by Ilya Stupakov
(<https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx>), int8-quantised, unmodified.

## Voice activity detection model (bundled)

**Silero VAD** (`crates/dictum-engine/assets/silero_vad.onnx`), from
<https://github.com/snakers4/silero-vad>.

```
MIT License

Copyright (c) 2020-present Silero Team

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Test fixtures

- `jfk.wav` — public-domain speech, as distributed with
  [whisper.cpp](https://github.com/ggml-org/whisper.cpp).
- `1272-128104-*.flac` — [LibriSpeech](https://www.openslr.org/12) dev-clean, CC BY 4.0.

## Runtime

[ONNX Runtime](https://github.com/microsoft/onnxruntime) (MIT), linked statically through the
[`ort`](https://github.com/pykeio/ort) crate (MIT/Apache-2.0).
