# HowlingWind Netplay Debug Notes & Research Data

## GNT4 Memory Map (Verified)

### Player Structs
- **P1 pointer**: `0x80226358` → dereference for P1 struct base
- **P2 pointer**: `0x80226614` → dereference for P2 struct base
- **Player struct size**: `0x300` bytes (768 bytes)
- **Health offset**: `+0x8E` (u16, big-endian) — this is a DAMAGE ACCUMULATOR (0 = full HP, >=150 = KO)
- **Buttons offset**: `+0x252` (u16) — CONFIRMED via fast input scan
- **Animation frame**: `+0x25A` (u16) — used for frame detection, CONFIRMED working in Practice mode
- **Stick X**: `+0x10` (i8) — needs verification
- **Stick Y**: `+0x11` (i8) — needs verification
- **C-Stick X**: `+0x12` (i8) — needs verification
- **C-Stick Y**: `+0x13` (i8) — needs verification
- **Trigger L**: `+0x14` (u8) — needs verification
- **Trigger R**: `+0x15` (u8) — needs verification

### PAD Buffer (Controller Input)
- **PAD buffer base**: `0x802233A0` — the SI polling buffer, where the game reads controller input BEFORE processing
- **PAD status size**: 12 bytes per port
- **Port layout**: Port 0 at +0, Port 1 at +12, Port 2 at +24, Port 3 at +36
- **PADStatus struct**: buttons(u16 BE) + stickX(i8) + stickY(i8) + cstickX(i8) + cstickY(i8) + trigL(u8) + trigR(u8)
- **CRITICAL**: Physical controller ALWAYS maps to port 0, regardless of player assignment
- **CRITICAL**: Write to PAD buffer, not player struct. Player struct button offsets are read-only (game output after processing)

### Global State
- **RNG seed**: Needs discovery — critical for desync prevention
- **Timer/frame counter**: P1 anim frame (+0x25A) is the reliable frame tick source
- **GC Timebase** (0x800000F8): NOT verified for GNT4, don't use
- **Scene frame counter** (0x800030D8): NOT verified, don't use

### Globals for State Snapshots
- Start: `0x80222DF0`
- Size: 1024 bytes (`0x400`)
- Contains: game timer, round state, etc.

## Bugs Found & Fixed (v0.1.0 → v0.1.6)

### Bug 1: UDP packets never sent (CRITICAL)
- **Cause**: `peer_addr` captured as `None` at `bind()` time, before `set_peer()` called
- **Fix**: `Arc<RwLock<Option<SocketAddr>>>` shared between send tasks and `set_peer()`

### Bug 2: Input read from wrong port (CRITICAL)
- **Cause**: Guest's `read_player_input(1)` reads from P2 player struct (empty in Dolphin)
- **Fix**: Always read from `read_pad_input(0)` (port 0 = physical controller)
- **Also**: Guest must redirect local port 0 input → port 1 PAD buffer (game expects P2 input there)

### Bug 3: Input injection to wrong memory
- **Cause**: `write_player_input()` writes to player struct offsets (read-only game output)
- **Fix**: `write_pad_buffer()` writes to PAD polling buffer (game input)

### Bug 4: KO detection inverted
- **Cause**: `health == 0` means FULL HP in GNT4 (damage accumulator)
- **Fix**: `health >= 150` for KO threshold

### Bug 5: Signal race condition
- **Cause**: `readyUp()` listener consumed `udp_ready` signals meant for `startMatch()`
- **Fix**: `filterType` parameter on `onSignals()`

### Bug 6: Auto-updater corrupted WebView
- **Cause**: Extracting zip directly over running app overwrites WebView DLLs
- **Fix**: Stage to `_update_staging/`, batch script applies after app exits

### Bug 7: White screen on distributed builds
- **Cause**: Missing `custom-protocol` Tauri feature — frontend not embedded
- **Fix**: `tauri = { version = "2", features = ["custom-protocol"] }`

## Architecture Notes for Dolphin Fork

### Current Approach (External Memory Manipulation)
- Attach to Dolphin process via `OpenProcess` + `ReadProcessMemory`/`WriteProcessMemory`
- Find GC RAM base by scanning for MEM1 signature (`0x00DFE007`)
- Read/write PAD buffer and player structs through process memory
- Frame detection: poll P1 animation frame counter every 500μs

### Limitations of External Approach
1. **No frame-level control** — can't pause Dolphin mid-frame for rollback resimulation
2. **Race conditions** — polling-based frame detection can miss frames or read mid-update
3. **No state restore** — lightweight snapshots (~2-4KB) can't restore full emulator state
4. **Menus don't sync** — player struct pointers invalid outside battles
5. **Can't inject code** — limited to memory read/write, no function hooks

### What a Fork Enables
1. **Frame callbacks** — hook into Dolphin's frame loop for precise timing
2. **Full save states** — emulator-level save/restore for true rollback
3. **Input pipeline control** — inject inputs at SI level before game sees them
4. **Desync detection** — hash full memory state, not just player structs
5. **Menu sync** — control game flow from emulator level
6. **Pipe/named pipe input** — Dolphin already supports this for TAS

### Slippi's Approach (Reference)
- Hooks into `SI_PollController()` for input injection
- Uses `State::SaveAs()` / `State::LoadAs()` for rollback (full save state)
- Custom EXI device for game ↔ emulator communication
- Melee-specific: reads game state from known Melee memory addresses
- `CEXISlippi` class handles all communication

### Minimum Fork Changes Needed
1. Hook frame callback in `Core/Core.cpp` → `Core::RunFrameFunc()`
2. Add custom save state ring buffer (keep N states in memory)
3. Hook SI polling → redirect to network input buffer
4. Add IPC mechanism (named pipe or shared memory) for launcher communication
5. Strip Melee-specific code from Slippi, keep rollback framework

## Network Architecture

### UDP Packet Format
- **Input packet**: 20 bytes (magic(4) + frame(4) + player_id(1) + input(8) + checksum(3))
- **Sync packet**: 16 bytes (magic(4) + frame(4) + state_hash(4) + unused(4))
- **Ping packet**: 12 bytes (magic(4) + timestamp_us(8))
- **Pong packet**: 12 bytes (magic(4) + timestamp_us(8))

### NAT Traversal
- STUN discovery via Google's public STUN servers
- UDP hole punching: send 3 packets 200ms apart to open NAT mapping
- Fallback: LAN IP for same-network play
- Fallback: direct connect by IP

### Signaling
- Firebase Realtime Database (free tier)
- Room-based: host creates room, guest joins by code
- Signals: `udp_ready` (with public address) and `start_game`

## Known Desync Risks
1. **RNG state divergence** — if RNG seed isn't saved/restored in snapshots
2. **Floating point determinism** — Dolphin's JIT may produce different FP results on different CPUs
3. **Timing differences** — frame detection polling can drift between machines
4. **Incomplete state snapshots** — current 2-4KB snapshots may miss game state
5. **Memory map differences** — different Dolphin versions/settings can shift addresses
