# GNT4 ISO Rollback Hack Plan

## Key Resources
- **GNTool**: https://github.com/NicholasMoser/Naruto-GNT-Modding (GNT-specific ISO tool)
- **doldecomp/gnt4**: https://github.com/doldecomp/gnt4 (active decompilation project)
- **dol_c_kit**: Write C code → compile to PPC → inject into DOL
- **Ghidra** with GameCube loader for static analysis

## GNT4 Memory Map
```
DOL Entry:     0x80003154
Code range:    0x80003100 - 0x801FD7FF
Data range:    0x801FD800 - 0x80222960
BSS range:     0x802229E0 - 0x8027C578
Free heap:     0x8027C578 - 0x817FFFFF (~22MB for rollback buffer)

P1 Base Ptr:   0x80226358
P2 Base Ptr:   0x80226614
Frame Counter: 0x8024A594
Input Range:   0x80222D40 - 0x8024C956

Input Handler: 0x80042C40
Hit Detection: 0x8003C95C
Block Handler: 0x8003A7E4
Pause Handler: 0x80047780
```

## Hybrid Approach (Recommended)
1. Keep Dolphin-side save states (reliable, already working)
2. Add 2 Gecko codes:
   - Input redirect hook at 0x80042C40 (read from shared memory instead of controller)
   - Frame boundary signal (write frame number to known address)
3. Inject via GNTool into a modded ISO copy
4. Rollback engine stays in Dolphin C++ code

## In-Game State to Save (~4-8KB)
- P1/P2 character structs: ~0x300 bytes each
- Global game state: ~0x1000 bytes
- Input buffers: ~0x2000 bytes
- RNG seed: 4 bytes
- SEQ script state: unknown (needs investigation)

## Balance Patches (via Gecko codes)
- Tenten: +5% damage
- Third Hokage: +5% damage
- Kakashi: +5% damage
- Gaara: +5% damage
- Orochimaru: Super can't be skipped (chakra drain persists)
