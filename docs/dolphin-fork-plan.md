# HowlingWind: Minimal Dolphin Fork Plan

## Executive Summary

This document details the **minimum viable Dolphin fork** needed to support rollback netplay for GNT4. The current approach (external process memory read/write from Tauri) works for lightweight game-state snapshots (~2-4KB) but fundamentally cannot do **true rollback** — that requires frame-stepping the emulator, full savestates, and precise input injection at the emulation level.

The plan is to fork Dolphin and add a small IPC interface that lets the HowlingWind Tauri launcher control frame advancement, savestates, and input injection. We keep Dolphin's normal GUI/rendering intact and only add a communication layer.

---

## Why a Fork is Necessary

The current external memory approach has hard limits:

1. **No frame stepping** — Dolphin runs its own frame loop. The Tauri app polls at ~1000Hz watching an animation counter, but cannot actually pause/advance/replay frames. During rollback, you need to replay N frames instantly (within one vsync), which requires control of the emulation loop.

2. **Incomplete savestates** — Saving ~2-4KB of player structs + globals is not a full savestate. Any state not captured (audio registers, GPU FIFO, CPU caches, RNG state in registers, interrupt timers) will desync on restore. Full emulator savestates capture everything.

3. **No deterministic replay** — Without frame-stepping, you cannot replay frames with corrected inputs. The current code writes inputs to memory, but the game may have already consumed the old input for that frame.

4. **Input timing** — Writing inputs to GC RAM from an external process has race conditions. The game reads the pad buffer at a specific point in the SI polling cycle. External writes can land at the wrong time.

---

## Dolphin Source Code Architecture (Key Files)

All paths relative to `Source/Core/` in the [Dolphin repository](https://github.com/dolphin-emu/dolphin).

### Emulation Loop & Frame Control

| File | Purpose |
|------|---------|
| `Core/Core.cpp` | Main emulation lifecycle. `EmuThread()` spawns the CPU thread. `DoFrameStep()` advances exactly one frame then pauses. `SetState()` controls pause/run. |
| `Core/Core.h` | Public API: `DoFrameStep()`, `SetState()`, `GetState()`, `PauseAndLock()` |
| `Core/System.cpp` | `Core::System` singleton that holds all subsystem references |
| `Core/HW/SystemTimers.cpp` | Schedules VI interrupts that drive frame boundaries |
| `Core/HW/VideoInterface.cpp` | `VideoInterfaceManager::Update()` fires every half-line. When half-line == 0 or == HalfLinesPerEvenField, it calls `MovieManager::FrameUpdate()` which increments `m_current_frame`. This is the canonical frame boundary. |

### Savestates

| File | Purpose |
|------|---------|
| `Core/State.cpp` | Full savestate implementation. `SaveToBuffer()` serializes entire emulator state to an in-memory `UniqueBuffer<u8>`. `DoState()` coordinates all subsystems (CPU, GPU, HW, audio). Uses LZ4 compression for disk, but the raw buffer path is already there. |
| `Core/State.h` | Public API: `Save()`, `Load()`, `SaveAs()`, `LoadAs()` |

Key insight: `SaveToBuffer()` already exists and writes to memory, not disk. The compression/disk-write happens in a separate step. We can intercept at the buffer level for fast in-memory savestates.

### Controller Input

| File | Purpose |
|------|---------|
| `Core/HW/SI/SI_DeviceGCController.cpp` | `CSIDevice_GCController::GetPadStatus()` — virtual method called when the SI hardware polls the controller. This is THE injection point. Also has `HandleMoviePadStatus()` for TAS playback. |
| `Core/HW/SI/SI_DeviceGCController.h` | Class with `GetPadStatus()`, `NetPlay_GetInput()` — already has a netplay input hook! |
| `Core/HW/GCPad.cpp` | `GCPad::GetStatus()` — higher-level input retrieval |
| `Core/HW/GCPadEmu.cpp` | Emulated pad (keyboard/controller mapped to GC pad) |
| `InputCommon/ControllerInterface/Pipes/Pipes.h` | **Named pipe input** — Dolphin already supports writing GC inputs via named pipes! Commands: `PRESS A`, `SET MAIN 0.5 0.5`, etc. Currently Unix-only. |
| `Core/Movie.cpp` | TAS input playback system. `RecordInput()`, `PlayController()` — another existing input injection mechanism. Has frame-accurate input via `m_current_frame`. |
| `Core/NetPlayClient.cpp` | Dolphin's built-in netplay. Has `NetPlay_GetInput()` which replaces local input with network input at the SI polling level. **This is exactly the pattern we need.** |

### Frame Counter

| File | Purpose |
|------|---------|
| `Core/Movie.h` | `MovieManager::GetCurrentFrame()` returns `m_current_frame` (u64). Incremented by `FrameUpdate()` called from `VideoInterface::Update()`. |

---

## Existing Dolphin Features We Can Leverage

### 1. Named Pipe Input (Pipes/)
Dolphin already has a pipe-based input system. A named pipe in `User/Pipes/` directory can receive commands like `PRESS A`, `RELEASE B`, `SET MAIN 0.5 0.3`. However:
- **Currently Unix-only** (uses POSIX file descriptors)
- Latency: reads once per `UpdateInput()` call, not frame-accurate
- Missing: no way to associate inputs with specific frames

Verdict: Useful reference code, but not precise enough for rollback. We need SI-level injection.

### 2. TAS/Movie System (Movie.cpp)
The movie system records and plays back inputs frame-by-frame. `PlayController()` replaces the controller state for a given frame. This is close to what we need but:
- Designed for pre-recorded playback, not live injection
- No external API — all internal

Verdict: Good architectural reference. The `PlayController` codepath shows exactly where to inject.

### 3. Built-in Netplay (NetPlayClient.cpp)
Dolphin has delay-based netplay. `NetPlay_GetInput()` is called from `SI_DeviceGCController` and replaces local input with network-received input. This is the exact injection pattern we need, but:
- Uses delay-based netcode (not rollback)
- No savestate integration
- Tightly coupled to Dolphin's UI/matchmaking

Verdict: **Best reference for input injection.** We mirror this pattern but add our own data source.

### 4. Frame Advance (Core.cpp)
`DoFrameStep()` is already fully implemented. It sets a flag, unpauses, waits for the next VI interrupt, then re-pauses. This is exactly what we need for rollback replay.

### 5. Python Scripting (Felk/dolphin fork)
An unofficial fork adds Python scripting with `event.on_frameadvance()` and `memory.read_u32()`. Not merged upstream. Too heavyweight for our needs, but proves the concept.

---

## How Slippi Did It

[Slippi](https://github.com/project-slippi/Ishiiruka) (based on the Ishiiruka fork of Dolphin) takes a Melee-specific approach:

1. **EXI Device** — Added a custom EXI (Expansion Interface) hardware device (`EXI_DeviceSlippi.cpp`). The game communicates with this virtual device via DMA reads/writes. This is how game data flows out and control signals flow in.

2. **Game-Side ASM** — Melee itself is patched with assembly code that writes frame data to the EXI device and reads opponent inputs from it. The game "cooperates" with the emulator.

3. **Savestate Integration** — `handleCaptureSavestate()` and `handleLoadSavestate()` in the EXI device trigger Dolphin's savestate system at precise moments.

4. **Rust Backend** — Recent Slippi versions use a Rust library (`slippi-rust-extensions`) linked into the C++ Dolphin build for the networking/rollback logic.

**Key difference from our approach:** Slippi patches the game ROM with custom assembly. We do NOT want to modify the GNT4 ISO. Instead, we control everything from the emulator side + external Tauri app.

The [BT3 Dolphin fork](https://github.com/PhantoomDev/bt3-dolphin) is attempting a similar approach for Dragon Ball Z BT3 — emulator-side rollback with game reverse engineering. They tag all changes with `// BT3 rollback` comments.

---

## The Minimal Fork: What to Change

### Overview of Changes

We add exactly **one new subsystem** to Dolphin: an IPC server that listens for commands from the HowlingWind Tauri launcher. Everything else uses existing Dolphin machinery.

```
┌─────────────────────┐     IPC (localhost TCP)     ┌──────────────────┐
│   HowlingWind       │ ◄─────────────────────────► │  Dolphin Fork    │
│   Tauri Launcher    │                              │                  │
│                     │  Commands:                   │  New files:      │
│  - Rollback engine  │  - SAVE_STATE <slot>         │  - HWServer.cpp  │
│  - Netplay (UDP)    │  - LOAD_STATE <slot>         │  - HWServer.h    │
│  - Matchmaking      │  - FRAME_ADVANCE             │                  │
│  - UI (Svelte)      │  - SET_INPUT <port> <data>   │  Modified:       │
│                     │  - GET_FRAME                  │  - Core.cpp      │
│                     │  - PAUSE / RESUME             │  - SI_Device*.cpp│
│                     │                              │  - State.cpp     │
│                     │  Events:                     │  - CMakeLists.txt│
│                     │  - FRAME_BOUNDARY <n>         │                  │
│                     │  - STATE_SAVED <slot>         │                  │
│                     │  - STATE_LOADED <slot>        │                  │
└─────────────────────┘                              └──────────────────┘
```

### New Files to Add

#### 1. `Source/Core/Core/HowlingWind/HWServer.h` — IPC Server Header

**Purpose:** Declare the IPC server interface.

**Contents:**
- `HWServer` class with `Start()`, `Stop()`, `ProcessCommands()`
- Command enum: `SaveState`, `LoadState`, `FrameAdvance`, `SetInput`, `GetFrame`, `Pause`, `Resume`
- Input injection buffer: `std::array<GCPadStatus, 4> m_injected_inputs`
- `bool m_input_override[4]` — per-port flag for whether to use injected input
- Savestate ring buffer: `std::array<UniqueBuffer<u8>, 16> m_state_ring`
- Frame boundary callback registration

**Complexity:** Medium

#### 2. `Source/Core/Core/HowlingWind/HWServer.cpp` — IPC Server Implementation

**Purpose:** TCP localhost server (port 17492 — "HOWL" in phone keypad) that accepts commands from the Tauri launcher.

**Protocol:** Simple line-based text protocol over TCP:
```
→ SAVE_STATE 0           # Save to in-memory slot 0
← OK SAVE_STATE 0 427831 # Saved, size in bytes

→ LOAD_STATE 0           # Load from slot 0
← OK LOAD_STATE 0        # Loaded

→ FRAME_ADVANCE          # Step exactly one frame
← OK FRAME 12345         # Now at frame 12345

→ SET_INPUT 1 0x0030 0 0 0 0 128 0  # Port 1: buttons=0x0030, sticks, triggers
← OK SET_INPUT 1

→ GET_FRAME              # Query current frame
← FRAME 12345

→ PAUSE                  # Pause emulation
← OK PAUSE

→ RESUME                 # Resume normal emulation
← OK RESUME

← EVENT FRAME_BOUNDARY 12346  # Server pushes frame events
```

**Implementation details:**
- Uses `std::thread` for the accept loop (Dolphin already uses threads extensively)
- Non-blocking reads on the command socket
- `ProcessCommands()` called from the CPU thread at frame boundaries
- For in-memory savestates: calls `State::SaveToBuffer()` directly, stores result in the ring buffer — skips compression and disk I/O entirely
- For loading: calls `State::LoadFromBuffer()` (we add this — inverse of `SaveToBuffer()`)

**Complexity:** Medium-Hard (the IPC itself is easy; integrating with Dolphin's thread model requires care)

#### 3. `Source/Core/Core/HowlingWind/HWInput.h` / `HWInput.cpp` — Input Override

**Purpose:** Provide a clean interface for overriding controller input at the SI polling level.

**Contents:**
- `SetInput(int port, GCPadStatus status)` — stores input for next poll
- `GetInput(int port)` -> `std::optional<GCPadStatus>` — returns override if set
- `ClearInput(int port)` — release override, return to normal controller
- Thread-safe (SI polling happens on CPU thread, IPC on its own thread)

**Complexity:** Easy

### Files to Modify

#### 4. `Source/Core/Core/State.cpp` — Add In-Memory Load

**What to change:**
- Add `LoadFromBuffer(Core::System& system, std::span<const u8> buffer)` — the inverse of the existing `SaveToBuffer()`. This deserializes a raw state buffer back into the emulator without touching disk.
- The existing code has `SaveToBuffer()` which writes to a `UniqueBuffer<u8>`. We add the matching load function that feeds a buffer into `DoState()` in read mode.

**Estimated diff:** ~40-60 lines

**Complexity:** Medium (need to understand the `PointerWrap` read vs write modes)

#### 5. `Source/Core/Core/State.h` — Expose New Function

**What to change:**
- Declare `LoadFromBuffer()`

**Estimated diff:** ~3 lines

**Complexity:** Easy

#### 6. `Source/Core/Core/HW/SI/SI_DeviceGCController.cpp` — Input Injection Hook

**What to change:**
- In `GetPadStatus()`, check the HowlingWind input override before reading the physical controller:
  ```cpp
  GCPadStatus CSIDevice_GCController::GetPadStatus()
  {
      // HowlingWind: check for injected input
      if (auto hw_input = HWInput::GetInput(m_device_number))
          return *hw_input;

      // Original code continues...
      GCPadStatus pad_status = {};
      // ...existing implementation...
  }
  ```
- This mirrors exactly how `NetPlay_GetInput()` already works — it replaces the pad status before the SI device processes it.

**Estimated diff:** ~10-15 lines

**Complexity:** Easy

#### 7. `Source/Core/Core/Core.cpp` — Frame Boundary Hook

**What to change:**
- In `Callback_NewField()` (called at each VI interrupt / frame boundary), add a hook that notifies HWServer of the new frame and processes any pending commands:
  ```cpp
  void Callback_NewField(Core::System& system)
  {
      // ...existing code...

      // HowlingWind: notify frame boundary
      if (auto* hw = system.GetHWServer())
          hw->OnFrameBoundary(movie_manager.GetCurrentFrame());
  }
  ```
- This gives us frame-accurate command processing.

**Estimated diff:** ~10 lines

**Complexity:** Easy

#### 8. `Source/Core/Core/System.cpp` / `System.h` — Register HWServer

**What to change:**
- Add `HWServer` as a member of the `Core::System` class
- Initialize it during boot, shut down on stop
- Add `GetHWServer()` accessor

**Estimated diff:** ~20 lines

**Complexity:** Easy

#### 9. `CMakeLists.txt` — Build Integration

**What to change:**
- Add the new `HowlingWind/` source files to the Core library build

**Estimated diff:** ~5 lines

**Complexity:** Easy

---

## IPC Protocol Choice: TCP Localhost

**Why TCP and not shared memory or named pipes:**

| Option | Pros | Cons |
|--------|------|------|
| **TCP localhost** | Cross-platform, debuggable (telnet), Tauri has great TCP support, no special permissions | ~0.1ms latency per message (negligible vs 16ms frame) |
| Shared memory | Fastest possible | Complex synchronization, platform-specific, hard to debug |
| Named pipes | Mid-ground | Windows named pipes are weird, Unix named pipes are different API |
| Unix domain socket | Fast, clean | Not on Windows |

TCP localhost adds ~0.1ms latency per round-trip. For rollback, the hot path is: receive FRAME_BOUNDARY event -> send SAVE_STATE -> receive OK -> continue. Total IPC overhead: ~0.3ms per frame, well within the 16.67ms budget.

For the **hot rollback path** (load state + replay N frames), the sequence is:
1. Tauri receives late remote input (~0ms, already in UDP buffer)
2. Tauri sends LOAD_STATE to Dolphin (~0.1ms)
3. Dolphin loads state from memory (~1-3ms for full state, no disk)
4. Tauri sends SET_INPUT + FRAME_ADVANCE in a loop for N frames (~0.1ms * N IPC + ~2ms * N emulation)
5. Total for 3-frame rollback: ~0.3ms IPC + 3ms load + 6ms replay = ~9.3ms (under 16ms budget)

---

## Savestate Performance

### Current: External Process Memory (~2-4KB, game state only)
- Save: ~0.05ms
- Load: ~0.05ms
- Correctness: **Incomplete** — misses GPU, audio, CPU registers, timers

### Fork: Full In-Memory Savestate (no compression, no disk)
Based on Dolphin's State.cpp architecture:
- `SaveToBuffer()` raw size: ~30-50MB (all subsystem state)
- Save time: ~2-5ms (memory copy, no compression)
- Load time: ~2-5ms
- Correctness: **Complete** — every register, every buffer

### Fork: Optimized (LZ4 in-memory compression)
- Compressed size: ~5-15MB (GC games have lots of zero-filled memory)
- Save: ~3-7ms (with fast LZ4)
- Load: ~2-5ms (LZ4 decompress is faster than compress)

### Recommended: Start Uncompressed
At 10 slots * 50MB = 500MB RAM for the ring buffer. Modern PCs have 16-32GB. This is fine. Compress later if needed.

---

## Build Instructions (Windows)

### Prerequisites
1. **Visual Studio 2022** (Community Edition is free)
   - Workloads: "Desktop development with C++"
   - Individual components: Latest MSVC build tools, Windows SDK, CMake tools
2. **Git for Windows**
3. **CMake 3.25+** (included with VS or install separately)

### Steps

```bash
# 1. Clone our fork (once we create it)
git clone --recursive https://github.com/YourOrg/howlingwind-dolphin.git
cd howlingwind-dolphin

# 2. Create build directory
mkdir build && cd build

# 3. Configure with CMake
cmake .. -G "Visual Studio 17 2022" -A x64

# 4. Build (Release for performance, RelWithDebInfo for debugging)
cmake --build . --config Release --parallel

# 5. Output binary will be in:
# build/Binaries/Release/Dolphin.exe
```

Alternative: Open `Source/dolphin-emu.sln` in Visual Studio and build from the IDE.

### Or with Ninja (faster incremental builds):
```bash
# Set up MSVC environment
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvarsall.bat" x64

mkdir build && cd build
cmake .. -GNinja -DCMAKE_BUILD_TYPE=Release
ninja
```

---

## Integration with HowlingWind Tauri App

### Current Architecture (to keep)
- `launcher/src-tauri/src/netplay.rs` — P2P UDP input exchange (good, keep as-is)
- `launcher/src-tauri/src/rollback.rs` — Rollback engine logic (refactor to use IPC)
- `launcher/src-tauri/src/dolphin_mem.rs` — External process memory (replace hot path with IPC)

### What Changes in the Tauri App

#### `dolphin_mem.rs` — Demote to Backup
- Keep the external memory reading for non-critical tasks (UI display of health bars, debug overlays)
- All rollback-critical operations (save/load state, input injection) move to IPC

#### New: `dolphin_ipc.rs` — IPC Client
```rust
// Connects to Dolphin's HWServer on localhost:17492
// Sends commands, receives events
pub struct DolphinIPC {
    stream: TcpStream,
    frame_rx: mpsc::Receiver<u64>,  // frame boundary events
}

impl DolphinIPC {
    pub fn connect() -> Result<Self, Error>;
    pub fn save_state(&mut self, slot: u8) -> Result<(), Error>;
    pub fn load_state(&mut self, slot: u8) -> Result<(), Error>;
    pub fn frame_advance(&mut self) -> Result<u64, Error>;  // returns new frame number
    pub fn set_input(&mut self, port: u8, input: &GCPadStatus) -> Result<(), Error>;
    pub fn get_frame(&mut self) -> Result<u64, Error>;
    pub fn pause(&mut self) -> Result<(), Error>;
    pub fn resume(&mut self) -> Result<(), Error>;
}
```

#### `rollback.rs` — Refactored Hot Path
The `run_game_loop()` function changes from passive polling to active frame control:

```
OLD (current): Poll memory at 1000Hz, detect frame change, react
NEW (with fork): Receive FRAME_BOUNDARY event, save state, exchange inputs,
                 if rollback needed: LOAD_STATE + SET_INPUT + FRAME_ADVANCE in loop
```

The rollback engine becomes the **driver** of frame advancement during rollback, rather than a passive observer. This is the fundamental architectural upgrade.

---

## Phased Implementation Plan

### Phase 1: Fork Setup & Build (1-2 days)
- Fork dolphin-emu/dolphin on GitHub
- Verify clean Windows build
- Add HowlingWind directory structure
- Add empty HWServer files that compile

### Phase 2: IPC Server (3-5 days)
- Implement TCP server with command parsing
- Wire up PAUSE, RESUME, GET_FRAME commands
- Test with telnet/netcat
- **Complexity: Medium**

### Phase 3: In-Memory Savestates (2-3 days)
- Add `LoadFromBuffer()` to State.cpp
- Implement SAVE_STATE / LOAD_STATE commands using raw buffers
- Ring buffer of 10-16 slots
- Benchmark save/load times
- **Complexity: Medium**

### Phase 4: Input Injection (1-2 days)
- Add HWInput module
- Hook into SI_DeviceGCController::GetPadStatus()
- Implement SET_INPUT command
- Test with simple input sequences
- **Complexity: Easy**

### Phase 5: Frame Stepping (2-3 days)
- Hook Callback_NewField for frame boundary events
- Implement FRAME_ADVANCE command (wraps DoFrameStep)
- Implement FRAME_BOUNDARY push events
- Test frame-accurate input + advance cycle
- **Complexity: Medium** (threading synchronization)

### Phase 6: Tauri Integration (3-5 days)
- Write `dolphin_ipc.rs` client
- Refactor `rollback.rs` to use IPC for save/load/advance
- Keep existing UDP netplay as-is
- End-to-end test: two instances, rollback working
- **Complexity: Medium**

### Total Estimated Time: 12-20 days of focused work

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Savestate load causes crash | Medium | High | Start with Dolphin's proven SaveToBuffer/DoState path. Test extensively in single-player first. |
| Frame stepping breaks game timing | Low | Medium | DoFrameStep() is already battle-tested by TAS community. |
| IPC latency too high for rollback | Low | Low | TCP localhost is ~0.1ms. Can upgrade to shared memory later. |
| Dolphin upstream changes break fork | Medium | Medium | Pin to a specific Dolphin release tag. Merge upstream periodically. |
| GNT4 has undiscovered state that desyncs | High | High | Full emulator savestates capture everything. This is the whole point of the fork. |
| Build complexity scares off contributors | Medium | Medium | Provide pre-built binaries. Most users never build from source. |

---

## File Summary

| File | Action | Lines Changed | Complexity |
|------|--------|--------------|------------|
| `Core/HowlingWind/HWServer.h` | **NEW** | ~80 | Medium |
| `Core/HowlingWind/HWServer.cpp` | **NEW** | ~400 | Medium-Hard |
| `Core/HowlingWind/HWInput.h` | **NEW** | ~30 | Easy |
| `Core/HowlingWind/HWInput.cpp` | **NEW** | ~60 | Easy |
| `Core/State.cpp` | Modify | ~50 | Medium |
| `Core/State.h` | Modify | ~3 | Easy |
| `Core/HW/SI/SI_DeviceGCController.cpp` | Modify | ~15 | Easy |
| `Core/Core.cpp` | Modify | ~10 | Easy |
| `Core/System.cpp` | Modify | ~15 | Easy |
| `Core/System.h` | Modify | ~5 | Easy |
| `CMakeLists.txt` | Modify | ~5 | Easy |
| **Total new code** | | **~570 lines** | |
| **Total modified** | | **~103 lines** | |

This is a genuinely minimal fork — ~670 lines of C++ changes to get full rollback infrastructure.

---

## References

- [Dolphin source: Core.cpp](https://github.com/dolphin-emu/dolphin/blob/master/Source/Core/Core/Core.cpp) — Emulation loop, DoFrameStep()
- [Dolphin source: State.cpp](https://github.com/dolphin-emu/dolphin/blob/master/Source/Core/Core/State.cpp) — Savestate system, SaveToBuffer()
- [Dolphin source: SI_DeviceGCController.h](https://github.com/dolphin-emu/dolphin/blob/master/Source/Core/Core/HW/SI/SI_DeviceGCController.h) — Controller polling, GetPadStatus()
- [Dolphin source: Movie.h](https://github.com/dolphin-emu/dolphin/blob/master/Source/Core/Core/Movie.h) — Frame counter, GetCurrentFrame()
- [Dolphin source: Pipes.h](https://github.com/dolphin-emu/dolphin/blob/master/Source/Core/InputCommon/ControllerInterface/Pipes/Pipes.h) — Named pipe input (reference)
- [Dolphin source: VideoInterface.cpp](https://github.com/dolphin-emu/dolphin/blob/master/Source/Core/Core/HW/VideoInterface.cpp) — VI interrupt / frame boundary
- [Dolphin Windows build guide](https://github.com/dolphin-emu/dolphin/wiki/Building-for-Windows)
- [Project Slippi Ishiiruka](https://github.com/project-slippi/Ishiiruka) — Slippi's Dolphin fork (Melee rollback reference)
- [BT3 Dolphin fork](https://github.com/PhantoomDev/bt3-dolphin) — BT3 rollback fork (closest parallel to our project)
- [Dolphin Python scripting PR](https://github.com/dolphin-emu/dolphin/pull/7064) — Felk's scripting API (reference)
- [Dolphin pipe input PR](https://github.com/dolphin-emu/dolphin/pull/3170) — Original named pipe implementation
