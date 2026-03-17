# HowlingWind Rollback Architecture

## Overview

Rollback netplay for GNT4 (and GNT Special) using a modified Dolphin emulator.
Goal: feel like local play at up to ~130ms ping.

## Architecture Layers

### Layer 1: P2P Networking (Tauri/Rust)
- UDP socket for input exchange (low latency, no head-of-line blocking)
- Firebase RTDB for signaling/matchmaking (room codes, WebRTC-style)
- Input packets: { frame_number, player_inputs, checksum }
- ~60 packets/sec per player (one per frame)

### Layer 2: Dolphin Memory Interface
- Read/write GNT4 game state via Dolphin's memory mapped regions
- Dolphin Memory Engine or direct process memory access
- Save state: snapshot critical memory regions
- Load state: restore snapshot for rollback

### Layer 3: Rollback Engine
- Input prediction: repeat last known input
- On receiving remote input: compare with prediction
- If mismatch: load state, replay frames with correct inputs
- Max rollback window: 7 frames (configurable)

## GNT4 Memory Map

### Player State (critical for rollback sync verification)
| Address     | Offset | Type   | Description |
|-------------|--------|--------|-------------|
| 0x80226358  | base   | ptr    | P1 base pointer |
| 0x80226614  | base   | ptr    | P2 base pointer |
| +0x262      | health | u16    | Health value |
| +0x28E      | chakra | u16    | Chakra (max 0x3C00) |
| +0x1DC      | vspeed | f32    | Vertical speed |
| +0x1E0      | grav   | f32    | Gravity (-0.083008 default) |
| +0x294      | block  | u16    | Block meter |

### Global State
| Address     | Type | Description |
|-------------|------|-------------|
| 0x8024A594  | u32  | Frame counter |
| 0x0400b304  | u32  | Match timer |

### Input Buffer
| Range | Description |
|-------|-------------|
| 0x80222D40 - 0x8024C956 | Controller input buffers |

### RNG
- Algorithm: LCG (seed = seed * 214013 + 2531011)
- Seed pointer: RAND_SEED_PTR (address TBD - needs runtime discovery)
- Output: (seed >> 16) & 0xFFFF

## Rollback Strategy

### Approach A: Full Memory Snapshot (Simple, ~5-10ms)
- Save/load entire GameCube RAM (24MB active)
- Use LZ4 for compression
- Pro: guaranteed correctness
- Con: slower, may cause micro-stutters on rollback

### Approach B: Delta Snapshots (Fast, ~1-2ms)
- Track dirty memory pages between frames
- Only save/load changed regions
- Pro: fast enough for imperceptible rollback
- Con: complex, needs MMU/page fault tracking in Dolphin

### Approach C: Selective State Save (Hybrid, ~2-4ms)
- Save only critical game state regions (~64KB total):
  - Player structs (4 x ~0x300 bytes = ~3KB)
  - Global game state (~1KB)
  - Input buffers (~2KB)
  - RNG seed (4 bytes)
  - Audio/visual state markers
  - Full GC RAM delta for remaining state
- Pro: fast for most frames, correct for all
- Con: needs careful identification of all state

### Recommended: Start with Approach A, optimize to C
- Get correctness first with full snapshots
- Profile and optimize hot path
- Community of 10-20 players will tolerate 5ms rollback initially
- Optimize to <2ms over time

## Input Prediction Algorithm

```
function predictInput(player, frame):
  // Simple: repeat last known input
  return lastKnownInput[player]

  // Advanced (future): pattern matching
  // Look at last N inputs, predict based on frequency
```

## Frame Synchronization Protocol

```
Frame N local:
  1. Read local input
  2. Send {frame: N, input: localInput} via UDP
  3. Check for remote input for frame N
  4. If remote input available:
     - Use it (no rollback needed)
  5. If not available:
     - Predict remote input
     - Mark frame N as "predicted"
  6. Advance emulation one frame
  7. Save state for frame N

When remote input for frame M arrives (M < currentFrame):
  8. Compare with prediction for frame M
  9. If prediction was wrong:
     - Load state for frame M
     - Replay frames M through currentFrame with correct inputs
     - Continue from corrected state
```

## Network Packet Format

```rust
struct InputPacket {
    magic: u32,          // 0x484F574C ("HOWL")
    frame: u32,          // frame number
    player_id: u8,       // 0 or 1
    inputs: u16,         // GC controller button state
    stick_x: i8,         // main stick X (-128 to 127)
    stick_y: i8,         // main stick Y
    cstick_x: i8,        // C-stick X
    cstick_y: i8,        // C-stick Y
    trigger_l: u8,       // L trigger analog
    trigger_r: u8,       // R trigger analog
    checksum: u32,       // CRC32 of game state hash (for desync detection)
}
// Total: 20 bytes per packet, 60 packets/sec = 1.2 KB/s bandwidth
```

## Phase Plan

### Phase 3A: P2P Input Exchange (Current)
- UDP socket in Rust backend
- Hole-punching via Firebase signaling
- Input packet serialization
- Input buffer with frame tracking

### Phase 3B: Dolphin Memory Access
- Read/write process memory from Tauri
- Save/load state snapshots
- Frame counter synchronization

### Phase 3C: Rollback Engine
- Input prediction
- State save ring buffer (7 frames)
- Mismatch detection
- State restore + replay

### Phase 4: Optimization
- Delta compression for save states
- Audio smoothing during rollback
- Visual interpolation
- Desync detection + recovery
