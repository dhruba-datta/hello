# Third-Party Notices

Hello's own source code is covered by the root [`LICENSE`](LICENSE). Rust and TypeScript
dependencies keep their own licenses, as recorded in `Cargo.lock` and each package's metadata.

## Bundled runtime libraries

The packaged app includes these libraries, each under the MIT License:

| Component | License |
|-----------|---------|
| ONNX Runtime 1.23.2 | MIT |
| Moonshine | MIT |
| Sparkle | MIT |
| transcribe.cpp (through the `transcribe-cpp` crate) | MIT |

## Speech models

Hello doesn't bundle speech models. They are downloaded on request, or placed by the user, and
each keeps its own license:

| Model | License |
|-------|---------|
| Whisper (base, large-v3-turbo) | MIT |
| Whisper large-v3-turbo Bangla fine-tune | Apache-2.0 |
| CAM++ speaker embedding model | Apache-2.0 |
| Parakeet, Moonshine and the other catalog models | See each model's source page |
