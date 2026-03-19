# Dolphin Sync Research — Key Findings for HowlingWind

## Why Rollback Isn't Working (Root Causes)

### 1. GetNetPads Blocks on Remote Input
The delay-based path blocks: `while (m_pad_buffer[pad_nb].Size() == 0) m_gc_pad_event.Wait()`
If rollback Init() fails, it falls through to this blocking path = deadlock or wrong behavior.

### 2. Full Savestates Too Slow
~30-40MB per frame is way too slow. Slippi uses `Memory::CopyFromEmu/CopyToEmu` for specific regions only (~100KB).

### 3. ENet Reliable Delivery Adds Latency
All pad data uses `ENET_PACKET_FLAG_RELIABLE` — guaranteed ordered delivery.
For rollback, use unreliable delivery with own sequence numbering.

## What Must Be Identical for Sync
- Byte-identical ISO
- Same Dolphin build
- Same settings (single core mandatory)
- SRAM synced (done by OnStartGame)
- Memory cards synced
- Initial RTC synced
- AR/Gecko codes synced
- CPU core: MUST be single-core (dual core = non-deterministic)

## Critical Technical Details

### Movie::GetCurrentFrame()
- VI field counter, increments at ~60Hz for NTSC
- Cannot skip, deterministic
- Reliable for frame indexing

### SI Polling
- GameCube can poll at 60Hz or 120Hz (game-dependent)
- GNT4 likely polls at 60Hz (standard)
- GetNetPads called via SI → GetPadStatus → NetPlay_GetInput
- batching=true on VI poll, batching=false on MMIO poll

### PollLocalPad
- Reads from GCAdapter::Input(local_pad) OR Pad::GetStatus(local_pad)
- local_pad → ingame_pad mapping via LocalPadToInGamePad()
- For 2P: Player 1's local_pad 0 → ingame_pad 0, Player 2's local_pad 0 → ingame_pad 1

### Pad Buffer (Delay-Based)
- SPSCQueue<GCPadStatus> per pad (dynamic size, not ring buffer)
- Push from OnPadData (network receive)
- Pop from GetNetPads (blocking wait)
- Buffer target = 20 frames default

## Slippi's Approach (What We Should Copy)
1. Save specific memory regions, NOT full Dolphin state
2. Use Memory::CopyFromEmu/CopyToEmu for fast saves (~100KB)
3. Exclude audio from savestates
4. Use EXI communication between game ASM and Dolphin
5. GGPO sync test mode: 1-frame rollback every frame in single player

## Action Items
1. Verify rollback Init() succeeds (check logs for pad mapping)
2. Add OSD overlay showing real-time rollback state (BUILD 4 — DONE)
3. Implement selective memory save (GNT4 regions only) instead of full savestate
4. Add GGPO sync test mode (single player forced rollback)
5. Consider unreliable ENet delivery for rollback pad data
6. Force single-core in netplay settings
