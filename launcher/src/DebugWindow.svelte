<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  let attached = $state(false);
  let scanning = $state(false);
  let fastScanning = $state(false);
  let debugData = $state<any>(null);
  let scanInterval = $state<number | null>(null);
  let log = $state<string[]>([]);

  function addLog(msg: string) {
    log = [...log.slice(-199), `[${new Date().toLocaleTimeString()}] ${msg}`];
  }

  async function attach() {
    try {
      const msg: string = await invoke("dolphin_mem_attach");
      attached = true;
      addLog(`Attached: ${msg}`);
      // Apply Gecko codes
      try { await invoke("dolphin_apply_gecko_live"); addLog("Gecko codes applied"); } catch {}
    } catch (e: any) {
      addLog(`Attach failed: ${e}`);
    }
  }

  async function runScan() {
    try {
      debugData = await invoke("dolphin_full_debug");
    } catch (e: any) {
      addLog(`Scan error: ${e}`);
    }
  }

  function toggleScan() {
    if (scanning) {
      if (scanInterval) { clearInterval(scanInterval); scanInterval = null; }
      scanning = false;
      addLog("Scan stopped");
    } else {
      scanning = true;
      addLog("Scan started (every 2s)");
      runScan();
      scanInterval = window.setInterval(runScan, 2000);
    }
  }

  async function runFastInputScan() {
    fastScanning = true;
    addLog("3 second delay... switch to game and start mashing buttons!");
    try {
      const result: string = await invoke("dolphin_fast_input_scan");
      addLog(`Fast scan done: ${result}`);
    } catch (e: any) {
      addLog(`Fast scan error: ${e}`);
    }
    fastScanning = false;
  }

  // Auto-attach on load
  let attachInterval = window.setInterval(async () => {
    if (attached) { clearInterval(attachInterval); return; }
    try {
      const msg: string = await invoke("dolphin_mem_attach");
      attached = true;
      addLog(`Auto-attached: ${msg}`);
      try { await invoke("dolphin_apply_gecko_live"); } catch {}
      clearInterval(attachInterval);
    } catch {}
  }, 2000);
</script>

<div class="debug-window">
  <div class="header">
    <h1>DEBUG CONSOLE</h1>
    <div class="status" class:on={attached}>{attached ? "DOLPHIN CONNECTED" : "WAITING FOR DOLPHIN..."}</div>
  </div>

  <div class="controls">
    {#if attached}
      <button class="btn scan-btn" onclick={toggleScan}>
        {scanning ? "STOP SCAN" : "START DEBUG"}
      </button>
      <button class="btn once-btn" onclick={runScan}>SCAN ONCE</button>
      <button class="btn fast-btn" onclick={runFastInputScan} disabled={fastScanning}>
        {fastScanning ? "SCANNING..." : "FAST INPUT SCAN"}
      </button>
    {:else}
      <button class="btn attach-btn" onclick={attach}>ATTACH NOW</button>
    {/if}
  </div>

  {#if debugData}
    <div class="hud">
      <div class="pills">
        <span class="pill" class:green={debugData.has_valid_players} class:red={!debugData.has_valid_players}>
          {debugData.has_valid_players ? "PLAYERS FOUND" : "NO PLAYERS"}
        </span>
        <span class="pill" class:green={debugData.gecko_applied} class:red={!debugData.gecko_applied}>
          {debugData.gecko_applied ? "UNLOCKS OK" : "UNLOCKS MISSING"}
        </span>
        <span class="pill neutral">FRAME {debugData.frame_counter}</span>
      </div>

      {#if debugData.has_valid_players}
        <div class="player-data">
          {#each debugData.lines as line}
            {#if line.startsWith("P1 ptr") || line.startsWith("P2 ptr")}
              <div class="pline ptr">{line}</div>
            {:else if line.startsWith("  chr=") || line.startsWith("  btn=")}
              <div class="pline data">{line}</div>
            {/if}
          {/each}
        </div>
      {/if}

      <details open>
        <summary>FULL OUTPUT ({debugData.lines.length} lines)</summary>
        <div class="full-output">
          {#each debugData.lines as line}
            <div
              class="outline"
              class:hdr={line.startsWith("═══")}
              class:warn={line.includes("⚠") || line.includes("MISMATCH") || line.includes("FAILED")}
              class:ok={line.includes("✓") || line.includes("OK") || line.includes("VALID")}
            >{line}</div>
          {/each}
        </div>
      </details>
    </div>
  {/if}

  <div class="log">
    <h3>LOG</h3>
    <div class="log-output">
      {#each log as line}
        <div class="log-line">{line}</div>
      {/each}
      {#if log.length === 0}
        <div class="log-line muted">Waiting...</div>
      {/if}
    </div>
  </div>
</div>

<style>
  :global(body) {
    margin: 0;
    background: #0a0e14;
    color: #e0e0e0;
    font-family: 'Courier New', monospace;
  }

  .debug-window {
    padding: 16px;
    height: 100vh;
    overflow-y: auto;
    box-sizing: border-box;
  }

  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 16px;
    padding-bottom: 12px;
    border-bottom: 1px solid rgba(34, 211, 238, 0.3);
  }

  h1 {
    font-family: 'Orbitron', monospace;
    font-size: 16px;
    font-weight: 700;
    letter-spacing: 3px;
    color: #22d3ee;
    margin: 0;
  }

  .status {
    font-family: 'Orbitron', monospace;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 1px;
    padding: 4px 12px;
    border-radius: 12px;
    background: rgba(239, 68, 68, 0.2);
    border: 1px solid rgba(239, 68, 68, 0.4);
    color: #ef4444;
  }

  .status.on {
    background: rgba(34, 197, 94, 0.2);
    border-color: rgba(34, 197, 94, 0.4);
    color: #22c55e;
  }

  .controls {
    display: flex;
    gap: 10px;
    margin-bottom: 16px;
  }

  .btn {
    padding: 8px 20px;
    font-family: 'Orbitron', monospace;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 1px;
    border-radius: 6px;
    cursor: pointer;
    transition: all 0.15s ease;
  }

  .scan-btn {
    background: linear-gradient(135deg, #3b82f6, #22d3ee);
    color: white;
    border: none;
  }

  .scan-btn:hover { box-shadow: 0 0 16px rgba(34, 211, 238, 0.4); }

  .once-btn {
    background: rgba(168, 85, 247, 0.2);
    color: #a855f7;
    border: 1px solid rgba(168, 85, 247, 0.4);
  }

  .once-btn:hover { background: rgba(168, 85, 247, 0.3); }

  .fast-btn {
    background: rgba(34, 197, 94, 0.2);
    color: #22c55e;
    border: 1px solid rgba(34, 197, 94, 0.4);
  }

  .fast-btn:hover:not(:disabled) { background: rgba(34, 197, 94, 0.3); }
  .fast-btn:disabled { opacity: 0.5; cursor: not-allowed; }

  .attach-btn {
    background: rgba(251, 191, 36, 0.2);
    color: #fbbf24;
    border: 1px solid rgba(251, 191, 36, 0.4);
  }

  .attach-btn:hover { background: rgba(251, 191, 36, 0.3); }

  .hud {
    background: rgba(0, 0, 0, 0.4);
    border: 1px solid rgba(34, 211, 238, 0.2);
    border-radius: 8px;
    padding: 12px;
    margin-bottom: 16px;
  }

  .pills {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    margin-bottom: 10px;
  }

  .pill {
    padding: 4px 10px;
    border-radius: 12px;
    font-family: 'Orbitron', monospace;
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 1px;
  }

  .pill.green { background: rgba(34, 197, 94, 0.2); border: 1px solid rgba(34, 197, 94, 0.4); color: #22c55e; }
  .pill.red { background: rgba(239, 68, 68, 0.2); border: 1px solid rgba(239, 68, 68, 0.4); color: #ef4444; }
  .pill.neutral { background: rgba(34, 211, 238, 0.1); border: 1px solid rgba(34, 211, 238, 0.3); color: #22d3ee; }

  .player-data {
    padding: 8px;
    background: rgba(0, 0, 0, 0.3);
    border-radius: 6px;
    margin-bottom: 10px;
  }

  .pline { font-size: 11px; line-height: 1.6; white-space: pre; }
  .pline.ptr { color: #22c55e; }
  .pline.data { color: #22d3ee; padding-left: 8px; }

  details { margin-top: 8px; }
  summary {
    font-family: 'Orbitron', monospace;
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 1px;
    color: rgba(255, 255, 255, 0.4);
    cursor: pointer;
    padding: 4px 0;
  }
  summary:hover { color: rgba(255, 255, 255, 0.7); }

  .full-output {
    margin-top: 6px;
    padding: 8px;
    background: rgba(0, 0, 0, 0.4);
    border-radius: 6px;
    max-height: 250px;
    overflow-y: auto;
  }

  .outline { font-size: 10px; line-height: 1.5; white-space: pre; color: rgba(255, 255, 255, 0.6); }
  .outline.hdr { color: #22d3ee; font-weight: 700; }
  .outline.warn { color: #f59e0b; }
  .outline.ok { color: #22c55e; }

  .log {
    background: rgba(0, 0, 0, 0.3);
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 8px;
    padding: 12px;
  }

  .log h3 {
    font-family: 'Orbitron', monospace;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 2px;
    color: rgba(255, 255, 255, 0.4);
    margin: 0 0 8px;
  }

  .log-output {
    max-height: 150px;
    overflow-y: auto;
  }

  .log-line { font-size: 10px; line-height: 1.5; color: rgba(255, 255, 255, 0.5); }
  .log-line.muted { font-style: italic; color: rgba(255, 255, 255, 0.3); }
</style>
