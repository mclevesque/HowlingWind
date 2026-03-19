# Rollback on Top of Dolphin's Existing Netplay: Implementation Plan

## Date: 2026-03-18
## Status: Research Complete, Ready for Implementation

---

## 1. Current State Assessment

### What We Have

**Dolphin's built-in netplay** (delay-based) handles:
- Connection establishment (direct IP + traversal server)
- ENet-based reliable/unreliable transport
- Pad data serialization and exchange
- Input buffering with configurable delay (`m_target_buffer_size`)
- Golf mode (host input authority)

**HowlingWind additions already in the fork:**
- `HWServer` (TCP IPC on port 17492) -- launcher<->Dolphin command interface
- `HWInput` -- SI-level input override (singleton, thread-safe)
- Frame boundary hook in `Core::Callback_NewField()` (line 910-912 of Core.cpp)
- `DoFrameStep()` integration for frame advance
- `State::Save/Load` called from IPC (currently uses disk-based slot saves -- too slow)

### What We Need to Change

The core problem: Dolphin's `NetPlayClient::GetNetPads()` is a **blocking** call. It waits on `m_pad_buffer[pad_nb]` to have data (line 2104). During rollback, we need to:
1. Not block -- use predicted inputs immediately
2. Correct predictions retroactively when real remote inputs arrive
3. Save/load state fast enough to resimulate within one frame time (~16.67ms)

### Key Constraint

We must keep Dolphin's existing traversal and connection infrastructure intact. Players still connect via `NetPlayClient` constructor (direct IP or traversal). We only replace the **input synchronization** and **frame advancement** logic.

---

## 2. Architecture: How Rollback Sits on Top of Netplay

```
 EXISTING (keep as-is)              NEW (add/modify)
 ========================           ========================
 NetPlayClient constructor          HWRollbackManager (new)
   - ENet connection                  - Input prediction
   - Traversal client                 - Savestate ring buffer
   - Direct IP                        - Rollback detection
                                      - Frame resimulation
 NetPlayServer
   - Session management             HWRollbackSavestate (new)
   - Pad map configuration            - In-memory DoState()
   - Game start sync                   - ~40MB buffer pool
                                       - <5ms save, <3ms load
 ENet transport layer
   - m_pad_buffer (repurposed)      Modified GetNetPads()
   - OnPadData (repurposed)           - Non-blocking
   - PollLocalPad (keep)              - Returns prediction if
                                        remote not ready
```

### Decision: Rollback Engine Lives in Dolphin, Not Tauri

The current architecture has the Tauri launcher driving rollback via IPC (SAVE_STATE, LOAD_STATE, FRAME_ADVANCE commands). This cannot work for real rollback because:
- IPC round-trip latency (~0.5-1ms per command) makes resimulating 7 frames take ~7-14ms just in IPC overhead
- Save/load via `State::Save(slot)` goes to disk -- takes 2+ seconds
- `DoFrameStep()` pauses the emulator and waits for a VI boundary -- cannot step fast enough

**The rollback engine must live inside Dolphin's CPU thread.** The Tauri launcher handles matchmaking and UI only. The Dolphin fork handles all frame-level rollback internally.

---

## 3. Detailed File-by-File Changes

### 3.1 New File: `Source/Core/Core/HowlingWind/HWRollback.h`

The central rollback state machine.

```cpp
namespace HowlingWind {

constexpr int ROLLBACK_MAX_FRAMES = 7;

enum class RollbackState {
  Normal,         // Running normally, saving states each frame
  Rollback,       // Loading state and resimulating
  Resimulating,   // Advancing frames during rollback
};

struct FrameInput {
  s32 frame;
  GCPadStatus local;       // Actual local input
  GCPadStatus remote;      // Actual or predicted remote input
  bool remote_confirmed;   // True if we have the real remote input
};

class HWRollback {
public:
  static HWRollback& GetInstance();

  // Called once when netplay game starts
  void Init(int local_player_index, int remote_player_index);
  void Shutdown();

  // Called from GetNetPads() replacement -- the core intercept point
  // Returns the input to use for the given pad on the current frame.
  // For the local player: reads real input and stores it.
  // For the remote player: returns confirmed input or prediction.
  GCPadStatus GetPadForFrame(int pad_nb, s32 frame);

  // Called when remote input arrives via existing netplay transport
  void OnRemoteInputReceived(s32 frame, const GCPadStatus& input);

  // Called at each frame boundary (from Callback_NewField)
  // Returns true if we should continue normal execution.
  // Returns false if we need to roll back (caller should initiate rollback).
  bool OnFrameBoundary(s32 frame);

  // Performs the rollback: load state, resimulate frames
  void ExecuteRollback(Core::System& system, s32 rollback_to_frame, s32 current_frame);

  bool IsRollingBack() const { return m_state == RollbackState::Resimulating; }
  bool IsActive() const { return m_active; }

  s32 GetCurrentFrame() const;
  s32 GetLastConfirmedRemoteFrame() const;

private:
  RollbackState m_state = RollbackState::Normal;
  bool m_active = false;

  int m_local_pad = 0;   // Which pad index is ours
  int m_remote_pad = 1;  // Which pad index is the opponent

  // Input history ring buffer (indexed by frame % (ROLLBACK_MAX_FRAMES + margin))
  static constexpr int INPUT_BUFFER_SIZE = ROLLBACK_MAX_FRAMES * 3;
  std::array<FrameInput, INPUT_BUFFER_SIZE> m_input_history;

  // Savestate pool
  std::array<std::unique_ptr<HWRollbackSavestate>, ROLLBACK_MAX_FRAMES + 1> m_savestates;
  std::map<s32, int> m_frame_to_state;  // frame -> savestate pool index

  s32 m_last_confirmed_remote_frame = -1;
  s32 m_oldest_unconfirmed_frame = 0;

  std::mutex m_input_mutex;
};

}  // namespace HowlingWind
```

### 3.2 New File: `Source/Core/Core/HowlingWind/HWRollbackSavestate.h/.cpp`

In-memory savestate that bypasses Dolphin's disk path. This is the most performance-critical component.

```cpp
namespace HowlingWind {

class HWRollbackSavestate {
public:
  HWRollbackSavestate();

  // Capture entire emulator state to memory buffer.
  // Must be called on CPU thread.
  // Target: <5ms for GC (24MB RAM + CPU/GPU state)
  void Capture(Core::System& system);

  // Restore from memory buffer.
  // Must be called on CPU thread.
  // Target: <3ms
  void Load(Core::System& system);

  bool IsValid() const { return m_valid; }
  s32 GetFrame() const { return m_frame; }
  void SetFrame(s32 frame) { m_frame = frame; }

private:
  Common::UniqueBuffer<u8> m_buffer;
  std::size_t m_used_size = 0;
  s32 m_frame = -1;
  bool m_valid = false;
};

}  // namespace HowlingWind
```

**Implementation strategy** -- reuse Dolphin's existing `SaveToBuffer()`/`LoadFromBuffer()`:

```cpp
void HWRollbackSavestate::Capture(Core::System& system) {
  // SaveToBuffer already exists in State.cpp (line 231) and writes
  // to an in-memory UniqueBuffer<u8> using PointerWrap.
  // It serializes: Movie, VideoBackend, CoreTiming, HW (all SI/EXI/etc),
  // PowerPC, Wiimote, Gecko, Achievements (see DoState() line 136-200)
  //
  // Initial buffer: allocate 40MB (typical GC state is ~30-35MB uncompressed)
  if (m_buffer.empty())
    m_buffer.reset(40 * 1024 * 1024);

  m_used_size = State::SaveToBuffer(system, m_buffer);  // <-- key call
  m_valid = (m_used_size > 0);
}

void HWRollbackSavestate::Load(Core::System& system) {
  if (!m_valid) return;
  State::LoadFromBuffer(system, std::span<u8>(m_buffer.data(), m_used_size));
}
```

**CRITICAL: These two functions (`SaveToBuffer`/`LoadFromBuffer`) are currently `static` in State.cpp.** We need to expose them.

### 3.3 Modify: `Source/Core/Core/State.h` (line 96-99)

Add new public API for in-memory save/load:

```cpp
// In-memory save/load for rollback (no compression, no disk, no netplay check)
std::size_t SaveToBuffer(Core::System& system, Common::UniqueBuffer<u8>& buffer);
bool LoadFromBuffer(Core::System& system, std::span<u8> buffer);
```

### 3.4 Modify: `Source/Core/Core/State.cpp`

**Change 1 (line 207-211):** Remove the netplay block on state loading for rollback:

Currently:
```cpp
if (NetPlay::IsNetPlayRunning())
{
  OSD::AddMessage("Loading savestates is disabled in Netplay to prevent desyncs");
  return false;
}
```

This blocks ALL state loads during netplay. We need rollback to load states while netplay is running. Change to:

```cpp
if (NetPlay::IsNetPlayRunning() && !HowlingWind::HWRollback::GetInstance().IsRollingBack())
{
  OSD::AddMessage("Loading savestates is disabled in Netplay to prevent desyncs");
  return false;
}
```

**Change 2 (line 222-257):** Make `SaveToBuffer()` and `LoadFromBuffer()` non-static (or add public wrapper functions that call the existing static versions). The static functions at lines 222 and 231 are exactly what we need -- they use `PointerWrap` to serialize/deserialize directly to memory without touching disk. No changes needed to their implementation, just visibility.

### 3.5 Modify: `Source/Core/Core/NetPlayClient.cpp` -- THE CORE CHANGE

**This is where delay-based becomes rollback.**

#### 3.5.1 Modify `GetNetPads()` (line 1991-2128)

The current flow:
1. If first pad and batching: poll all local pads, send to network
2. Wait on `m_pad_buffer[pad_nb]` until remote input arrives (BLOCKING)
3. Pop from buffer, return

The new flow:
1. If rollback is active (`HWRollback::IsActive()`):
   a. For local pad: read real input, store in rollback history, send to remote
   b. For remote pad: check if confirmed input exists for this frame
      - If yes: use it
      - If no: predict (repeat last known input)
   c. NEVER BLOCK -- return immediately with prediction
2. If rollback is NOT active: fall through to existing delay-based code (backwards compatible)

```cpp
// Line ~1991 in NetPlayClient.cpp
bool NetPlayClient::GetNetPads(const int pad_nb, const bool batching, GCPadStatus* pad_status)
{
  auto& rollback = HowlingWind::HWRollback::GetInstance();

  // === ROLLBACK PATH ===
  if (rollback.IsActive())
  {
    s32 frame = static_cast<s32>(Core::System::GetInstance().GetMovie().GetCurrentFrame());

    // Still need to send local input to remote via existing transport
    if (IsFirstInGamePad(pad_nb) && batching && !rollback.IsRollingBack())
    {
      // Poll local pads and send via existing ENet transport
      sf::Packet packet;
      packet << MessageID::PadData;
      const int num_local_pads = NumLocalPads();
      for (int local_pad = 0; local_pad < num_local_pads; local_pad++)
      {
        PollLocalPad(local_pad, packet);
      }
      SendAsync(std::move(packet));
    }

    // Get input from rollback manager (predicted or confirmed)
    *pad_status = rollback.GetPadForFrame(pad_nb, frame);
    return true;
  }

  // === EXISTING DELAY-BASED PATH (unchanged) ===
  // ... (all existing code from line ~2020 onwards stays as-is)
}
```

#### 3.5.2 Modify `OnPadData()` (line 673-693)

Currently pushes remote pad data into `m_pad_buffer`. We need to ALSO feed it to the rollback manager:

```cpp
void NetPlayClient::OnPadData(sf::Packet& packet)
{
  auto& rollback = HowlingWind::HWRollback::GetInstance();

  while (!packet.endOfPacket())
  {
    PadIndex map;
    packet >> map;

    GCPadStatus pad;
    packet >> pad.button;
    if (!m_gba_config.at(map).enabled)
    {
      packet >> pad.analogA >> pad.analogB >> pad.stickX >> pad.stickY >>
          pad.substickX >> pad.substickY >> pad.triggerLeft >> pad.triggerRight >>
          pad.isConnected;
    }

    if (rollback.IsActive())
    {
      // Feed to rollback manager with frame number
      // NOTE: We need to add frame number to pad packets (see section 3.6)
      rollback.OnRemoteInputReceived(frame_from_packet, pad);
    }
    else
    {
      // Existing delay-based path
      m_pad_buffer.at(map).Push(pad);
      m_gc_pad_event.Set();
    }
  }
}
```

#### 3.5.3 Pad Packet Frame Number

The existing `PadData` packet format does NOT include a frame number -- it's just (map, buttons, sticks, triggers). For rollback, we MUST know which frame each input belongs to.

**Option A (minimal change):** Add a new message type `MessageID::RollbackPadData` that includes the frame number. The server passes it through unchanged.

**Option B (Slippi approach):** Use a completely separate UDP channel for rollback inputs, bypassing ENet entirely. Faster but more work.

**Recommended: Option A.** Add frame-tagged pad data alongside existing pad data. This keeps the ENet transport and traversal working.

### 3.6 Modify: `Source/Core/Core/NetPlayServer.cpp`

Minimal change needed. The server currently relays `PadData` messages to all clients. For the new `RollbackPadData` message, it just needs to relay it the same way. Add a case in the server's message handler (around line 400-600 in the message processing switch) to forward `RollbackPadData` packets unchanged.

### 3.7 Modify: `Source/Core/Core/Core.cpp` -- Frame Boundary Hook

The existing frame boundary hook (lines 910-912) sends frame events to the IPC server. We need to add rollback logic here:

```cpp
// In Callback_NewField() (line 889)
void Callback_NewField(Core::System& system)
{
  // Existing frame step logic (lines 891-906) -- keep as-is
  if (s_frame_step) { ... }

  AchievementManager::GetInstance().DoFrame();

  // === ROLLBACK FRAME BOUNDARY ===
  auto& rollback = HowlingWind::HWRollback::GetInstance();
  if (rollback.IsActive() && !rollback.IsRollingBack())
  {
    s32 frame = static_cast<s32>(system.GetMovie().GetCurrentFrame());

    // Save state for this frame
    rollback.CaptureStateForFrame(system, frame);

    // Check if we need to roll back
    s32 rollback_to = rollback.CheckForRollback(frame);
    if (rollback_to >= 0)
    {
      rollback.ExecuteRollback(system, rollback_to, frame);
    }
  }

  // HowlingWind: notify IPC server of frame boundary (existing)
  if (s_hw_server && s_hw_server->IsRunning())
    s_hw_server->OnFrameBoundary(system.GetMovie().GetCurrentFrame());
}
```

### 3.8 Modify: `Source/Core/Core/HW/SI/SI_DeviceGCController.cpp`

The existing `GetPadStatus()` (line 142-167) already has our HWInput override at the top. During rollback resimulation, inputs come from the rollback manager rather than from the network or physical controller.

**The existing HWInput hook can serve this purpose.** During resimulation, the rollback engine sets inputs via `HWInput::SetInput()` before each resimulated frame, and the SI poll picks them up at line 147-148. No additional changes needed here.

However, there is one issue: `HandleMoviePadStatus()` at line 159 calls `NetPlay_GetInput()` which calls `GetNetPads()` which would try to read/send over the network during resimulation. We need to ensure that during resimulation, `GetNetPads()` returns from the rollback path and does NOT send network packets (the `!rollback.IsRollingBack()` check in section 3.5.1 handles this).

### 3.9 Modify: `Source/Core/Core/Movie.cpp`

No changes needed to Movie.cpp. The frame counter (`m_current_frame`) is included in the savestate via `DoState()` (line 975), so it will be correctly restored on rollback. The `FrameUpdate()` call at line 173 increments it normally during resimulation.

---

## 4. In-Memory Savestate Performance Analysis

### Current State (Dolphin mainline SaveToBuffer/LoadFromBuffer)

`State::DoState()` serializes (in order, from State.cpp lines 136-200):
1. Wii/GC mode flag
2. Memory sizes (MEM1 + MEM2)
3. Movie state (frame counter, etc.)
4. Video backend (GPU FIFO, textures, EFB copies)
5. CoreTiming (event queue, global ticks)
6. Hardware (SI, EXI, AudioInterface, PI, DSP, DVD, Memory, etc.)
7. PowerPC (registers, caches, BATs, page tables)
8. Wiimote state (not applicable for GC)
9. Gecko codes
10. Achievements

For GameCube, the dominant cost is:
- **MEM1 RAM**: 24MB (always fully serialized)
- **CPU state**: ~few KB (registers, caches)
- **Video backend**: variable, 1-10MB depending on texture cache
- **Other HW**: ~few KB each

Total uncompressed size: **~30-35MB typical** for GC.

### Performance Targets

At 60fps, one frame = 16.67ms. For 7 frames of rollback:
- We need to save state once per frame: **must be <5ms**
- On rollback, load + resimulate 7 frames: **must be <16ms total**
  - Load: <3ms
  - Each resimulated frame: ~1-2ms (CPU only, skip rendering)
  - 7 frames: ~7-14ms
  - Total: ~10-17ms

### Optimization Strategy (Phase 1: Get It Working)

Use `SaveToBuffer`/`LoadFromBuffer` directly. These do a `memcpy` of all RAM plus PointerWrap serialization. On modern hardware (DDR4/DDR5), copying 35MB takes ~2-4ms. This is borderline acceptable.

### Optimization Strategy (Phase 2: Make It Fast)

**Approach: Selective State Saving (like Slippi)**

Slippi's `SlippiSavestate` does NOT use Dolphin's full `DoState()`. Instead, it copies specific memory regions directly via `Memory::CopyFromEmu()`/`CopyToEmu()`. For SSBM, this is ~10MB of game-relevant memory. It explicitly excludes audio and XFB (video framebuffer) regions.

For GNT4, we can do the same:
1. Copy MEM1 RAM (24MB) -- cannot avoid this, game state is everywhere
2. Copy CPU registers (PowerPC state) -- few KB
3. Copy CoreTiming state -- event queue
4. **Skip video backend** -- GPU textures/EFB are re-rendered during resimulation
5. **Skip audio** -- mute/smooth during rollback frames
6. **Skip DSP state** -- audio DSP, not gameplay-relevant

This reduces savestate size to ~24MB + overhead, and eliminates the video backend serialization which can be the slowest part.

### Optimization Strategy (Phase 3: Dirty Page Tracking)

Reference: [dolphin-emu/dolphin#12911](https://github.com/dolphin-emu/dolphin/pull/12911) -- "Track dirty pages for speeding up sequential savestates."

Between frames, typically only a few KB of RAM actually changes. Using page-fault tracking (mprotect on Linux, VirtualProtect on Windows), we can detect which 4KB pages were written and only save those. This reduces per-frame save cost from ~24MB to ~50-200KB, bringing save time down to <0.5ms.

This is a Phase 3 optimization. Get full-copy working first.

---

## 5. Frame Resimulation via CPU Thread

### How `DoFrameStep()` Works (and Why We Cannot Use It)

`DoFrameStep()` (Core.cpp line 1024) does:
1. Sets `s_frame_step = true`
2. Calls `SetState(Running)` to unpause
3. The CPU thread runs until `Callback_FramePresented()` sets `s_stop_frame_step = true`
4. `Callback_NewField()` sees `s_stop_frame_step` and calls `CPU::Break()`

This involves: thread synchronization, GPU queue wait, state change notifications, and UI updates. **Far too slow for resimulation** where we need to advance 7 frames in <14ms.

### How Slippi Does Resimulation

Slippi does NOT step frames from outside the CPU thread. Instead, the rollback is triggered **from within the CPU thread** during the frame boundary callback. The sequence is:

1. CPU thread is at frame N's boundary (VI interrupt)
2. Rollback detects misprediction going back to frame M
3. **On the CPU thread:** load savestate for frame M
4. **On the CPU thread:** run the CPU loop forward (7 frames)
5. During these 7 frames, inputs come from the rollback history buffer
6. Video rendering is suppressed (no SwapBuffers) during resimulation
7. After catching up to frame N, resume normal execution

### Our Implementation: `ExecuteRollback()`

```cpp
void HWRollback::ExecuteRollback(Core::System& system, s32 target_frame, s32 current_frame)
{
  m_state = RollbackState::Rollback;

  // 1. Load savestate for target_frame
  int state_idx = m_frame_to_state[target_frame];
  m_savestates[state_idx]->Load(system);

  // 2. Set up inputs for resimulation
  //    For each frame from target_frame to current_frame:
  //    - Local input: from history buffer (we recorded it)
  //    - Remote input: from confirmed inputs (we have them now)

  m_state = RollbackState::Resimulating;

  // 3. Suppress video output during resimulation
  //    (set a flag that the video backend checks)
  g_rollback_skip_present = true;

  // 4. Run CPU forward for (current_frame - target_frame) frames
  //    We're already on the CPU thread, so we can call the CPU
  //    execution loop directly.
  s32 frames_to_resimulate = current_frame - target_frame;
  for (s32 i = 0; i < frames_to_resimulate; i++)
  {
    s32 resim_frame = target_frame + i;

    // Set inputs for this frame via HWInput
    auto& input = m_input_history[resim_frame % INPUT_BUFFER_SIZE];
    HWInput::GetInstance().SetInput(m_local_pad, input.local);
    HWInput::GetInstance().SetInput(m_remote_pad, input.remote);

    // Run one frame of CPU execution
    // This calls into the PowerPC interpreter/JIT until the next VI interrupt
    RunOneFrame(system);
  }

  // 5. Re-enable video output
  g_rollback_skip_present = false;

  // 6. Clear HWInput overrides for normal execution
  HWInput::GetInstance().ClearAll();

  m_state = RollbackState::Normal;
}
```

### `RunOneFrame()` -- The Missing Piece

This function needs to run the CPU until one VI interrupt fires. Dolphin's CPU execution happens in `PowerPC::RunLoop()` (PowerPC.cpp). During normal execution, the CPU thread loops there indefinitely. We need a version that runs until exactly one frame boundary.

```cpp
void HWRollback::RunOneFrame(Core::System& system)
{
  // Save the current frame count
  s32 start_frame = static_cast<s32>(system.GetMovie().GetCurrentFrame());

  // Run the CPU until the frame counter increments
  auto& cpu = system.GetCPU();
  auto& power_pc = system.GetPowerPC();

  // Use SingleStep mode to advance until frame boundary
  while (static_cast<s32>(system.GetMovie().GetCurrentFrame()) == start_frame)
  {
    // Execute one timeslice (~1000 cycles)
    power_pc.SingleStep();
  }
}
```

**Alternative approach** (more robust): Use CoreTiming to calculate ticks until the next VI interrupt, then run `PowerPC::Run()` for exactly that many ticks. This avoids the polling loop:

```cpp
void HWRollback::RunOneFrame(Core::System& system)
{
  auto& core_timing = system.GetCoreTiming();
  auto& power_pc = system.GetPowerPC();

  // CoreTiming knows when the next VI interrupt is scheduled.
  // Run the CPU until that event fires.
  // The VI interrupt handler will call FrameUpdate() which increments m_current_frame.

  s64 ticks_to_vi = core_timing.GetTicksToNextVIInterrupt();
  power_pc.RunFor(ticks_to_vi);
}
```

Note: `RunFor()` does not exist in vanilla Dolphin. We would need to add it. However, `PowerPC::SingleStep()` exists and runs one instruction. A middle ground is to use the existing `CoreTiming::Advance()` mechanism -- the CPU thread calls `CoreTiming::Advance()` which processes events up to the current time and runs CPU slices between them. We can call it in a loop until the frame counter increments.

**Recommended for Phase 1:** Use the simple frame-counter polling loop with `SingleStep()`. It's correct and simple. Optimize later.

---

## 6. Keeping Existing Traversal/Connection Working

### What NOT to Change

| Component | File | Change? |
|-----------|------|---------|
| ENet host creation | NetPlayClient.cpp:134-148 | NO |
| Traversal client connection | NetPlayClient.cpp:184-200 | NO |
| Server creation | NetPlayServer.cpp:121-178 | NO |
| Connection handshake | NetPlayClient.cpp:Connect() | NO |
| Game digest verification | NetPlayClient.cpp | NO |
| Pad mapping | NetPlayServer.cpp | NO |
| Session settings sync | NetPlayServer.cpp | NO |
| Golf mode | NetPlayClient.cpp | NO (disable in rollback mode) |

### New Config Option

Add a netplay setting to enable rollback mode:

```cpp
// In Config/NetplaySettings.h
extern const Info<bool> NETPLAY_ROLLBACK_ENABLED;
extern const Info<int> NETPLAY_ROLLBACK_MAX_FRAMES;
```

When `NETPLAY_ROLLBACK_ENABLED` is false, everything behaves exactly as vanilla Dolphin. When true, `GetNetPads()` takes the rollback path.

### Connection Flow (Unchanged)

1. Player A starts a NetPlay server (direct IP or traversal)
2. Player B connects via NetPlayClient
3. Settings sync, game digest verified
4. Host starts the game
5. **NEW:** Both clients detect rollback mode enabled, initialize `HWRollback::Init()`
6. Game runs with rollback-modified `GetNetPads()`

### Message Flow

Existing delay-based messages continue to work for the handshake and sync. During gameplay:
- `MessageID::PadData` -- still used for backward compatibility if rollback is disabled
- `MessageID::HWRollbackPadData` (NEW) -- includes frame number, used when rollback is enabled

---

## 7. Minimum Code Changes Summary

### New Files (4)

| File | Purpose | Lines (est.) |
|------|---------|------|
| `Core/HowlingWind/HWRollback.h` | Rollback state machine, input history, prediction | ~100 |
| `Core/HowlingWind/HWRollback.cpp` | Core rollback logic, savestate management, resimulation | ~400 |
| `Core/HowlingWind/HWRollbackSavestate.h` | In-memory savestate wrapper | ~40 |
| `Core/HowlingWind/HWRollbackSavestate.cpp` | Capture/Load using State::SaveToBuffer/LoadFromBuffer | ~80 |

### Modified Files (6)

| File | Change | Lines Changed (est.) |
|------|--------|-----|
| `Core/State.h` | Expose `SaveToBuffer()`/`LoadFromBuffer()` as public API | +3 |
| `Core/State.cpp` | Remove static from SaveToBuffer/LoadFromBuffer, allow state load during rollback | ~10 |
| `Core/NetPlayClient.cpp` | Rollback path in `GetNetPads()`, frame-tagged input in `OnPadData()` | ~60 |
| `Core/NetPlayServer.cpp` | Relay new `RollbackPadData` message | ~10 |
| `Core/Core.cpp` | Rollback check at frame boundary in `Callback_NewField()` | ~20 |
| `Core/NetPlayClient.h` | New `MessageID` enum entry | +2 |

### Unchanged Files

- `SI_DeviceGCController.cpp` -- HWInput hook already handles resimulation inputs
- `Movie.cpp`/`Movie.h` -- frame counter serialized via DoState, works automatically
- `NetPlayServer.cpp` -- connection/traversal logic untouched
- All `Config/` files -- just adding new config entries

**Total estimated new/modified code: ~725 lines**

---

## 8. Step-by-Step Implementation Order

### Step 1: In-Memory Savestates (Foundation)

**Goal:** Save and load emulator state to memory in <5ms.

Files to modify:
- `Source/Core/Core/State.h` -- add public `SaveToBuffer()`/`LoadFromBuffer()` declarations
- `Source/Core/Core/State.cpp` -- make existing static functions accessible, bypass netplay check

Files to create:
- `Source/Core/Core/HowlingWind/HWRollbackSavestate.h`
- `Source/Core/Core/HowlingWind/HWRollbackSavestate.cpp`

**Test:** Call `HWRollbackSavestate::Capture()` and `Load()` from the existing HWServer IPC interface (replace the current `State::Save(slot)` call). Measure timing. Verify state restores correctly (game continues from saved point without desync).

### Step 2: Frame-Tagged Input Transport

**Goal:** Remote inputs arrive with frame numbers attached.

Files to modify:
- `Source/Core/Core/NetPlayClient.h` -- add `MessageID::HWRollbackPadData`
- `Source/Core/Core/NetPlayClient.cpp` -- new send/receive path for frame-tagged inputs
- `Source/Core/Core/NetPlayServer.cpp` -- relay the new message type

**Test:** Two Dolphin instances connected via netplay. Verify frame-tagged inputs arrive correctly. Existing delay-based mode still works when rollback is disabled.

### Step 3: Rollback State Machine

**Goal:** The rollback manager tracks inputs, detects mispredictions, triggers rollbacks.

Files to create:
- `Source/Core/Core/HowlingWind/HWRollback.h`
- `Source/Core/Core/HowlingWind/HWRollback.cpp`

Files to modify:
- `Source/Core/Core/NetPlayClient.cpp` -- `GetNetPads()` rollback path
- `Source/Core/Core/Core.cpp` -- frame boundary rollback check

**Test:** Connect two instances, intentionally introduce latency. Verify:
- Inputs are predicted when remote is late
- Predictions use last-known-input repeat
- Mispredictions are detected when real input differs

### Step 4: Frame Resimulation

**Goal:** On misprediction, load state and replay frames.

Files to modify:
- `Source/Core/Core/HowlingWind/HWRollback.cpp` -- `ExecuteRollback()` + `RunOneFrame()`
- `Source/Core/Core/State.cpp` -- allow LoadFromBuffer during netplay rollback

**Test:** Two instances with artificial 100ms delay. Verify:
- Game plays smoothly for local player
- Remote player's character corrects after brief prediction
- No desyncs after 5+ minutes of play
- Rollback resimulation completes within 16ms

### Step 5: Video Suppression During Resimulation

**Goal:** Don't render intermediate frames during rollback (avoids visual glitches).

Files to modify:
- Video backend: add a global `g_rollback_skip_present` flag
- `Source/Core/VideoCommon/Present.cpp` or equivalent -- skip `SwapBuffers()` when flag is set

**Test:** Rollback with 7 frames of resimulation. No flickering or frame skips visible.

### Step 6: Audio Handling During Rollback

**Goal:** Prevent audio glitches during resimulation.

Options:
- **Simple:** Mute audio during resimulation frames (adds brief silence on rollback)
- **Better:** Buffer audio and smooth playback (Slippi approach)

For Phase 1, mute during resimulation.

### Step 7: Config UI and Polish

- Add rollback toggle to netplay settings dialog
- Add rollback frame count slider (1-7, default 7)
- Add OSD indicator showing rollback state (frame delay, rollback count)
- Add desync detection (compare game state checksums between players)

---

## 9. Risk Analysis

### Risk 1: SaveToBuffer Performance

**Concern:** Full `DoState()` may take >5ms on slower hardware.
**Mitigation:** Profile on target hardware. If too slow, implement selective state saving (skip video backend) as Phase 2.

### Risk 2: Video Backend State During Resimulation

**Concern:** Video backend's `DoState()` may be slow or cause issues when loading mid-frame.
**Mitigation:** For Phase 1, include full video state in savestate. For Phase 2, skip video state and accept that textures may flash briefly on rollback. Slippi skips video state entirely.

### Risk 3: Determinism

**Concern:** GNT4 may have nondeterministic behavior (floating point, RNG seeded from real-time).
**Mitigation:** Dolphin's existing determinism guarantees (via netplay mode) should handle this. Netplay mode already forces deterministic settings. Full savestates capture complete CPU/memory state, so even if there is nondeterminism, the rollback restores to an exact state.

### Risk 4: Thread Safety

**Concern:** `OnRemoteInputReceived()` is called from the ENet network thread. `GetPadForFrame()` is called from the CPU thread. `ExecuteRollback()` runs on the CPU thread.
**Mitigation:** Use a mutex for the input history buffer. Keep rollback execution exclusively on the CPU thread. The existing `crit_netplay_client` mutex pattern works fine.

### Risk 5: Audio Desync on Rollback

**Concern:** Loading a savestate resets audio state. Resimulating frames generates audio that should not be played.
**Mitigation:** Phase 1: mute during resimulation. Phase 2: implement audio ringbuffer that holds audio until confirmed (Slippi's approach).

---

## 10. Slippi Reference Summary

Key learnings from Slippi's implementation:

| Aspect | Slippi Approach | Our Approach |
|--------|----------------|--------------|
| State capture | Custom: copies specific RAM regions via `CopyFromEmu()`, skips audio/video | Phase 1: full `SaveToBuffer()`. Phase 2: selective like Slippi |
| State pool | `availableSavestates` + `activeSavestates` map | Fixed-size array indexed by frame % pool_size |
| Input prediction | Last-input repeat from `remotePadQueue` | Same: last-input repeat |
| Rollback trigger | Game's ASM code signals via EXI when inputs mismatch | Dolphin-side: compare predicted vs confirmed in `OnRemoteInputReceived()` |
| Resimulation | CPU thread runs forward from loaded state | Same: CPU thread RunOneFrame() loop |
| Video during resim | Skip rendering | Same: `g_rollback_skip_present` flag |
| Network transport | Custom UDP via EXI device | Reuse ENet (Dolphin's existing netplay transport) |
| Max rollback frames | 7 (ROLLBACK_MAX_FRAMES) | 7 (configurable) |

Key difference: Slippi uses game-specific ASM hooks to trigger save/load at exact game frames. GNT4 does not have these hooks. We trigger from Dolphin's VI interrupt (frame boundary) instead, which is more general and works for any game.

---

## 11. File Path Reference

All paths relative to `C:\Users\Thehu\Projects\HowlingWind\howlingwind-dolphin\`:

### Files to Create
- `Source/Core/Core/HowlingWind/HWRollback.h`
- `Source/Core/Core/HowlingWind/HWRollback.cpp`
- `Source/Core/Core/HowlingWind/HWRollbackSavestate.h`
- `Source/Core/Core/HowlingWind/HWRollbackSavestate.cpp`

### Files to Modify
- `Source/Core/Core/State.h` (line 96-99: add SaveToBuffer/LoadFromBuffer declarations)
- `Source/Core/Core/State.cpp` (line 207-211: bypass netplay check for rollback; line 222,231: expose static functions)
- `Source/Core/Core/NetPlayClient.cpp` (line 1991: rollback path in GetNetPads; line 673: feed OnPadData to rollback)
- `Source/Core/Core/NetPlayClient.h` (add MessageID for rollback pad data)
- `Source/Core/Core/NetPlayServer.cpp` (relay RollbackPadData message)
- `Source/Core/Core/Core.cpp` (line 889: rollback check in Callback_NewField)

### Files Already Modified (keep as-is)
- `Source/Core/Core/HowlingWind/HWServer.h` -- IPC server for Tauri launcher
- `Source/Core/Core/HowlingWind/HWServer.cpp` -- IPC command handlers
- `Source/Core/Core/HowlingWind/HWInput.h` -- SI-level input override
- `Source/Core/Core/HowlingWind/HWInput.cpp` -- Input override implementation
- `Source/Core/Core/HW/SI/SI_DeviceGCController.cpp` (line 147: HWInput hook)
- `Source/Core/Core/Core.cpp` (line 108: HWServer instance; line 682: server start; line 910-912: frame boundary hook)

### CMakeLists.txt
- `Source/Core/Core/CMakeLists.txt` -- add new HowlingWind source files to build
