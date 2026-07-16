# OpenVoice

**Local-first voice typing for macOS.** Hold a shortcut, speak, and text appears at your cursor — fully offline.

## Why it exists

OpenVoice is a lean, privacy-first alternative to cloud dictation. Audio never leaves your machine. Models run locally via Whisper (and optionally Parakeet).

## How it works

1. Grant **Microphone** + **Accessibility** (guided on first launch)
2. Download **Tiny** (75 MB) or **Base** (142 MB)
3. Hold `⌘/Ctrl + ⇧ + Space`, speak, release
4. Text is typed at the cursor (or copied to the clipboard)

## Features

- **Warm model cache** — the active Whisper model stays in memory so transcription is fast after the first run
- **Light defaults** — Tiny first; heavy models (Medium, Parakeet) hidden until you ask
- **Onboarding** — permissions + model download before you hit a dead end
- **Push-to-talk or toggle** recording modes
- **Menu bar app** — stays out of the Dock (accessory policy)
- **Floating HUD** while recording / transcribing

## Dev

```bash
# Requires: Rust, Node, Xcode CLT
# Optional for Parakeet: brew install onnxruntime

npm install
npm run tauri dev
```

Models and settings live at:

```
~/Library/Application Support/com.openvoice.app/
```

## Stack

- **Tauri 2** + React + TypeScript
- **whisper-rs** (CoreML-capable) with in-process model cache
- **parakeet-rs** + ONNX Runtime (optional advanced path)
- **cpal** audio capture, **enigo** typing, global shortcuts

## License

Private / WIP unless otherwise stated.
