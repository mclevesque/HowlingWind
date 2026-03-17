<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
  import { playSfx } from "../lib/audio";

  let phase = $state<"idle" | "launching">("idle");
  let errorMsg = $state("");
  let debugWin = $state<WebviewWindow | null>(null);

  async function startDebugPlay() {
    phase = "launching";
    errorMsg = "";

    try {
      // Scan for games
      const games: any[] = await invoke("scan_games");
      const gnt4 = games.find((g: any) => g.id === "gnt4");
      if (!gnt4) {
        errorMsg = "GNT4 ISO not found. Place it in the games/ folder and set Dolphin path in Settings.";
        phase = "idle";
        return;
      }

      // Open debug console window FIRST
      try {
        // Close old debug window if it exists
        if (debugWin) {
          try { await debugWin.close(); } catch {}
        }
        const win = new WebviewWindow("debug", {
          title: "HowlingWind — Debug Console",
          url: "index.html#debug-window",
          width: 450,
          height: 700,
          resizable: true,
          alwaysOnTop: true,
          x: 50,
          y: 50,
        });
        debugWin = win;
      } catch (e: any) {
        errorMsg = `Failed to open debug window: ${e}`;
        phase = "idle";
        return;
      }

      // Launch Dolphin
      await invoke("launch_dolphin", { mode: "practice", isoOverride: gnt4.iso_path });
      playSfx("match_found");
      phase = "idle";
    } catch (e: any) {
      errorMsg = `Launch failed: ${e}`;
      phase = "idle";
    }
  }
</script>

<div class="debug-play">
  <h2 class="page-title">DEBUG PLAY</h2>
  <p class="page-desc">Launch GNT4 + a separate debug console window</p>

  <div class="section">
    <p class="info">This launches Dolphin with GNT4 and opens a <strong>separate debug window</strong> that stays on top. The debug window auto-attaches to Dolphin and lets you scan game memory while playing.</p>

    <button class="btn-play" onclick={startDebugPlay} disabled={phase === "launching"}>
      {phase === "launching" ? "LAUNCHING..." : "PLAY IN DEBUG MODE"}
    </button>

    {#if errorMsg}
      <div class="error">{errorMsg}</div>
    {/if}
  </div>

  <div class="section tips">
    <h3 class="section-title">HOW IT WORKS</h3>
    <ol>
      <li>Click the button above — Dolphin launches + a debug console opens</li>
      <li>Navigate to VS mode and start a fight</li>
      <li>In the debug console, click <strong>START DEBUG</strong></li>
      <li>Live player data (health, chakra, inputs, frame counter) appears in real-time</li>
      <li>Debug report is saved to <code>debug_report.txt</code> automatically</li>
    </ol>
  </div>
</div>

<style>
  .debug-play {
    padding: 32px 40px;
  }

  .page-title {
    font-family: 'Orbitron', monospace;
    font-size: 24px;
    font-weight: 700;
    letter-spacing: 3px;
  }

  .page-desc {
    color: var(--text-secondary);
    margin-top: 4px;
    font-size: 14px;
  }

  .section {
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 20px;
    margin-top: 20px;
  }

  .section-title {
    font-family: 'Orbitron', monospace;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 2px;
    color: var(--wind-cyan);
    margin-bottom: 12px;
  }

  .info {
    font-size: 14px;
    color: var(--text-secondary);
    margin-bottom: 16px;
    line-height: 1.5;
  }

  .info strong {
    color: var(--wind-cyan);
  }

  .btn-play {
    width: 100%;
    padding: 20px 32px;
    background: linear-gradient(135deg, var(--accent-primary), var(--wind-cyan));
    color: white;
    font-family: 'Orbitron', monospace;
    font-size: 16px;
    font-weight: 700;
    letter-spacing: 3px;
    border-radius: 12px;
    transition: all 0.2s ease;
  }

  .btn-play:hover:not(:disabled) {
    box-shadow: 0 0 30px rgba(34, 211, 238, 0.3);
    transform: translateY(-2px);
  }

  .btn-play:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .error {
    margin-top: 12px;
    padding: 10px 14px;
    background: rgba(239, 68, 68, 0.1);
    border: 1px solid rgba(239, 68, 68, 0.3);
    border-radius: 8px;
    color: #ef4444;
    font-size: 13px;
  }

  .tips ol {
    list-style: none;
    counter-reset: steps;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .tips li {
    counter-increment: steps;
    font-size: 13px;
    color: var(--text-secondary);
    padding-left: 28px;
    position: relative;
    line-height: 1.5;
  }

  .tips li::before {
    content: counter(steps);
    position: absolute;
    left: 0;
    width: 20px;
    height: 20px;
    background: rgba(34, 211, 238, 0.15);
    color: var(--wind-cyan);
    font-family: 'Orbitron', monospace;
    font-size: 10px;
    font-weight: 700;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .tips li strong {
    color: var(--text-primary);
  }

  .tips li code {
    color: var(--wind-cyan);
    background: rgba(34, 211, 238, 0.1);
    padding: 1px 6px;
    border-radius: 3px;
    font-size: 12px;
  }
</style>
