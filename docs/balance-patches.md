# HowlingWind Balance Patches

Applied at runtime via memory manipulation — no ISO modification needed.
The launcher reads this config and applies damage multipliers during matches.

## Global Modifiers

| Character | Game ID | Damage Multiplier | Notes |
|-----------|---------|-------------------|-------|
| Tenten | TBD | 1.05 (5% buff) | |
| Third Hokage (Hiruzen Sarutobi) | TBD | 1.05 (5% buff) | |
| Kakashi | TBD | 1.05 (5% buff) | |
| Gaara | TBD | 1.05 (5% buff) | |

## Gameplay Fixes

| Fix | Description | Priority |
|-----|-------------|----------|
| Orochimaru super skip exploit | If Orochimaru's super connects, opponent starts next round with 0 chakra even if the super animation is skipped. Currently skipping negates the chakra drain. | High |

## Implementation Plan

Once the fork is ready, balance patches work by:
1. Hooking into the damage calculation in GNT4 memory
2. When damage is written to a player's health accumulator, multiply by the attacker's modifier
3. Applied symmetrically — both players see the same modified damage (no desync)
4. Can be toggled on/off per lobby (ranked vs casual)

## Character IDs (GNT4)
Need to map character select IDs to these names. Will fill in during RE phase.
Known memory: character ID is stored in the player struct (offset TBD).

## Orochimaru Super Fix — Technical Details

When Orochimaru's super connects and the round ends:
1. Detect Orochimaru's super activation via animation state in memory
2. Track if the super "connected" (hit the opponent) by checking hitstun/damage
3. On round end, if super connected, force opponent's chakra to 0 for next round
4. Applied via IPC memory write regardless of animation skip status
5. Both clients apply the same logic = no desync

## Change Log
- 2026-03-17: Initial balance list (Tenten, Third Hokage, Kakashi, Gaara +5%)
- 2026-03-17: Added Orochimaru super skip fix
