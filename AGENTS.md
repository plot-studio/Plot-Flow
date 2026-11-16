# AGENTS.md

Guidance for AI coding assistants working on Plot Flow.

Plot Flow is a Windows desktop dictation app by Plot Studio: press a shortcut,
speak, and the recognised text is inserted into the active application. Speech
recognition runs fully on the device (bundled GigaAM v3 model). Built with
Tauri 2.x (Rust backend + React/TypeScript frontend).

Repository: https://github.com/plot-studio/Plot-Flow (public on purpose: the
installer link in README.md downloads from its releases).

## Development Commands

**Prerequisites:**

- [Rust](https://rustup.rs/) (latest stable)
- Node.js + npm
- Visual Studio C++ Build Tools and (optionally) the Vulkan SDK for Windows builds

**Core development:**

```bash
npm install               # Install dependencies
npm run tauri dev         # Run in development mode
npm run dev               # Frontend only (Vite dev server)
npm run build             # Build frontend (TypeScript + Vite)
```

**Windows installer:**

```powershell
scripts\prepare-model.ps1   # Download and verify the bundled GigaAM model (SHA256-checked)
scripts\build-windows.ps1   # Full NSIS build with model and native runtimes
scripts\dev-build.ps1       # Faster local build for testing
```

**Linting and formatting (run before committing):**

```bash
npm run lint              # ESLint for frontend
npm run lint:fix          # ESLint with auto-fix
npm run format            # Prettier + cargo fmt
npm run format:check      # Check formatting without changes
```

The Silero VAD model (`src-tauri/resources/models/silero_vad_v4.onnx`) is
tracked in the repository. The GigaAM model is large, kept out of git and
fetched by `scripts\prepare-model.ps1`.

## Architecture Overview

### Backend (src-tauri/src/)

- `lib.rs` - Main entry point, Tauri setup, manager initialization
- `managers/` - Core business logic:
  - `audio.rs` - Audio recording and device management
  - `model.rs` - Model management
  - `transcription.rs` - Speech-to-text processing pipeline
  - `history.rs` - Transcription history storage
- `audio_toolkit/` - Low-level audio processing:
  - `audio/` - Device enumeration, recording, resampling
  - `vad/` - Voice Activity Detection (Silero VAD)
- `commands/` - Tauri command handlers for frontend communication
- `cli.rs` - CLI argument definitions (clap derive)
- `shortcut/` - Global keyboard shortcuts (`native_keys.rs` low-level hook,
  `tauri_impl.rs` Tauri global-shortcut plugin, `handler.rs` shared logic)
- `settings.rs` - Application settings management
- `overlay.rs` - Recording overlay window
- `portable.rs` - Portable mode (`portable` marker + `Data/` next to the exe)
- `signal_handle.rs` - `send_transcription_input()` reusable function
- `utils.rs` - Platform detection and environment helpers

### Frontend (src/)

- `App.tsx` - Main component with onboarding flow
- `components/` - React UI components (`settings/`, `onboarding/`,
  `overlay/`, `update-checker/`, `shared/`, `ui/`, `icons/`, `footer/`)
- `hooks/useSettings.ts` - Settings state management hook
- `stores/settingsStore.ts` - Zustand store for settings
- `bindings.ts` - Auto-generated Tauri type bindings (via tauri-specta)
- `overlay/` - Recording overlay window entry point
- `lib/types.ts` - Shared TypeScript type definitions

### Key patterns

- **Managers:** Audio, Model and Transcription managers are initialized at
  startup and held in Tauri state.
- **Commands and events:** frontend → backend via Tauri commands; backend →
  frontend via events.
- **Pipeline:** Audio → VAD → GigaAM → text → clipboard/paste into the active app.
- **State flow:** Zustand → Tauri command → Rust state → persistence
  (tauri-plugin-store).

### Application flow

1. The app starts minimized to the tray, loads settings, initializes managers.
2. A global shortcut starts recording; VAD trims silence.
3. Audio is transcribed locally by the bundled model.
4. Text is inserted into the active application.

### Persisted data — keep compatible

Settings (`settings_store.json`), `history.db`, `statistics.db` and the
`Data\` folder of portable installs live on users' machines. Never rename a
persisted key, enum value or file name without a serde alias or a one-time
migration. Existing examples: `KeyboardImplementation::NativeKeys` accepts the
legacy value via `#[serde(alias)]`; `portable.rs` still accepts the legacy
portable marker; `history.rs` migrates legacy recording file names.

## Internationalization (i18n)

All user-facing strings must use i18next translations. ESLint enforces this
(no hardcoded strings in JSX).

1. Add the key to `src/i18n/locales/en/translation.json` (and `ru`).
2. Use it in a component: `const { t } = useTranslation(); t('key.path')`.

The tray menu strings are generated from the `tray` section of the locale files
by `src-tauri/build.rs`.

## Code Style

**Rust:** run `cargo fmt` and `cargo clippy`; handle errors explicitly (avoid
`unwrap` in production code); doc comments for public APIs.

**TypeScript/React:** strict TypeScript, no `any`; functional components with
hooks; Tailwind CSS; path alias `@/` → `./src/`.

## CLI Parameters

**Implementation:** `cli.rs` (definitions), `main.rs` (parsing), `lib.rs`
(applying), `signal_handle.rs` (shared logic).

| Flag                     | Description                                                |
| ------------------------ | ---------------------------------------------------------- |
| `--toggle-transcription` | Toggle recording on/off on a running instance              |
| `--toggle-post-process`  | Toggle recording with post-processing on/off               |
| `--cancel`               | Cancel the current operation on a running instance         |
| `--start-hidden`         | Launch without showing the main window (tray icon visible) |
| `--no-tray`              | Launch without system tray (closing window quits the app)  |
| `--debug`                | Enable debug mode with verbose (Trace) logging             |

CLI flags are runtime-only overrides and never modify persisted settings.
Remote-control flags work through `tauri_plugin_single_instance`: a second
instance forwards its arguments to the running one and exits.

## Environment switches

Runtime switches use the `PLOT_FLOW_` prefix and are read through
`utils::env_var` / `utils::env_flag_enabled`, which also honour the pre-rebrand
prefix as a silent fallback.

- `PLOT_FLOW_KEEP_VULKAN_IMPLICIT_LAYERS=1` - keep implicit Vulkan layers
  (overlays, capture hooks), which are otherwise disabled for the process via
  `VK_LOADER_LAYERS_DISABLE=~implicit~` in `main.rs`.
- `PLOT_FLOW_NO_GTK_LAYER_SHELL=1` - Linux: skip GTK layer shell for the overlay.
- `PLOT_FLOW_METAL_RESIDENCY=1` - macOS: restore ggml Metal residency sets.
- Build-time: `PLOT_FLOW_VC_REDIST_DIRS` (set by the build scripts to stage the
  VC++ runtime DLLs), `PLOT_FLOW_FORCE_AI_STUB=1` (macOS).

## Debug Mode

`Ctrl+Shift+D` opens the debug settings.

## Commits

Commit messages are in Russian and describe why the change was made. Never
commit `.env`, signing keys, model files or anything over ~50 MB.
