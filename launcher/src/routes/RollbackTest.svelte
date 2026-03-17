<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { playSfx } from "../lib/audio";

  let attached = $state(false);
  let attachMsg = $state("");
  let testLog = $state<string[]>([]);
  let debugRunning = $state(false);
  let debugResult = $state<any>(null);
  let autoRunning = $state(false);
  let autoInterval: number | null = null;

  // Rollback test state
  let rollbackRunning = $state(false);
  let rollbackStats = $state<any>(null);
  let statsInterval: number | null = null;
  let saveTestResult = $state("");
  let saveTestRunning = $state(false);

  function log(msg: string) {
    testLog = [...testLog.slice(-199), `[${new Date().toLocaleTimeString()}] ${msg}`];
  }

  async function attachDolphin() {
    try {
      const msg: string = await invoke("dolphin_mem_attach");
      attached = true;
      attachMsg = msg;
      log(`Attached: ${msg}`);
      try { await invoke("dolphin_apply_gecko_live"); log("Gecko codes applied"); } catch {}
      playSfx("match_found");
    } catch (e: any) {
      attachMsg = `Error: ${e}`;
      log(`Attach failed: ${e}`);
    }
  }

  async function detachDolphin() {
    await invoke("dolphin_mem_detach");
    attached = false;
    attachMsg = "";
    debugResult = null;
    log("Detached from Dolphin");
  }

  async function runFullDebug() {
    debugRunning = true;
    try {
      debugResult = await invoke("dolphin_full_debug");
      log("=== DEBUG SCAN COMPLETE ===");
      if (debugResult.has_valid_players) {
        log("Players VALID — game is in a fight!");
      } else {
        log("No valid player pointers — start a fight first");
      }
      playSfx("click");
    } catch (e: any) {
      log(`Debug failed: ${e}`);
    }
    debugRunning = false;
  }

  function startAutoScan() {
    autoRunning = true;
    log("Auto-scan started (every 3s)");
    autoInterval = window.setInterval(async () => {
      try { debugResult = await invoke("dolphin_full_debug"); } catch {}
    }, 3000);
  }

  function stopAutoScan() {
    autoRunning = false;
    if (autoInterval) { clearInterval(autoInterval); autoInterval = null; }
    log("Auto-scan stopped");
  }

  // ── Save/Load State Test ──

  async function testSaveLoad() {
    saveTestRunning = true;
    saveTestResult = "";
    log("Testing save/load state speed...");

    try {
      const result: any = await invoke("test_save_load_speed");
      saveTestResult = `Save: ${result.save_ms.toFixed(2)}ms | Load: ${result.load_ms.toFixed(2)}ms | Size: ${(result.size_bytes / 1024 / 1024).toFixed(1)}MB`;
      log(`Save state: ${result.save_ms.toFixed(2)}ms`);
      log(`Load state: ${result.load_ms.toFixed(2)}ms`);
      log(`State size: ${(result.size_bytes / 1024 / 1024).toFixed(1)}MB`);
      if (result.save_ms < 5 && result.load_ms < 5) {
        log("EXCELLENT — fast enough for rollback!");
      } else if (result.save_ms < 16 && result.load_ms < 16) {
        log("OK — within one frame budget (16.67ms)");
      } else {
        log("WARNING — too slow for smooth rollback");
      }
      playSfx("click");
    } catch (e: any) {
      saveTestResult = `Error: ${e}`;
      log(`Save/load test failed: ${e}`);
    }
    saveTestRunning = false;
  }

  // ── Local Rollback Test ──
  // Starts the rollback engine in "local test" mode:
  // Runs the full rollback loop against a single Dolphin instance.
  // Reads P1 inputs, sends them through a localhost UDP loopback,
  // and verifies the rollback engine processes them correctly.

  async function startRollbackTest() {
    log("Starting local rollback test...");
    try {
      // Start netplay session on localhost
      const port: number = await invoke("netplay_start", {
        playerId: 0,
        inputDelay: 2,
        maxRollback: 7,
        port: 0,
      });
      log(`UDP session bound on port ${port}`);

      // Connect to ourselves (loopback)
      await invoke("netplay_connect", { peerAddress: `127.0.0.1:${port}` });
      log("Connected to localhost loopback");

      // Start rollback engine
      await invoke("rollback_start", {
        inputDelay: 2,
        maxRollback: 7,
        localPlayer: 0,
        ranked: false,
      });

      rollbackRunning = true;
      log("Rollback engine RUNNING — play the game and watch the stats!");
      playSfx("match_found");

      // Poll stats
      statsInterval = window.setInterval(async () => {
        try {
          rollbackStats = await invoke("rollback_stats");
        } catch {}
      }, 250);
    } catch (e: any) {
      log(`Rollback test failed: ${e}`);
    }
  }

  async function stopRollbackTest() {
    try {
      await invoke("rollback_stop");
      await invoke("netplay_stop");
    } catch {}
    rollbackRunning = false;
    rollbackStats = null;
    if (statsInterval) { clearInterval(statsInterval); statsInterval = null; }
    log("Rollback engine stopped");
  }
</script>

<div class="test-page">
  <h2 class="page-title">ROLLBACK TEST LAB</h2>
  <p class="page-desc">Test memory access, save states, and rollback engine</p>

  <!-- Step 1: Connect -->
  <div class="section">
    <h3 class="section-title">1. CONNECT TO DOLPHIN</h3>
    <div class="button-row">
      {#if !attached}
        <button class="btn-primary" onclick={attachDolphin}>ATTACH TO DOLPHIN</button>
      {:else}
        <span class="dolphin-found">DOLPHIN CONNECTED</span>
        <button class="btn-danger" onclick={detachDolphin}>DETACH</button>
      {/if}
    </div>
    {#if attachMsg}
      <div class="status-msg" class:ok={attached} class:err={!attached}>{attachMsg}</div>
    {/if}
  </div>

  {#if attached}
    <!-- Step 2: Debug scan -->
    <div class="section">
      <h3 class="section-title">2. VERIFY GAME STATE</h3>
      <div class="button-row">
        <button class="btn-primary" onclick={runFullDebug} disabled={debugRunning}>
          {debugRunning ? "SCANNING..." : "SCAN"}
        </button>
        {#if !autoRunning}
          <button class="btn-secondary" onclick={startAutoScan}>AUTO (3s)</button>
        {:else}
          <button class="btn-danger" onclick={stopAutoScan}>STOP AUTO</button>
        {/if}
      </div>

      {#if debugResult}
        <div class="debug-status">
          <div class="status-pill" class:green={debugResult.has_valid_players} class:red={!debugResult.has_valid_players}>
            {debugResult.has_valid_players ? "PLAYERS FOUND" : "NO PLAYERS"}
          </div>
          <div class="status-pill" class:green={debugResult.gecko_applied} class:red={!debugResult.gecko_applied}>
            {debugResult.gecko_applied ? "UNLOCKS OK" : "NO UNLOCKS"}
          </div>
          <div class="status-pill neutral">FRAME: {debugResult.frame_counter}</div>
        </div>
      {/if}
    </div>

    <!-- Step 3: Save/Load test -->
    <div class="section">
      <h3 class="section-title">3. SAVE/LOAD STATE SPEED</h3>
      <p class="section-desc">Tests how fast we can snapshot and restore 32MB of GC RAM. Needs &lt;16ms for rollback.</p>
      <div class="button-row">
        <button class="btn-primary" onclick={testSaveLoad} disabled={saveTestRunning}>
          {saveTestRunning ? "TESTING..." : "TEST SAVE/LOAD"}
        </button>
      </div>
      {#if saveTestResult}
        <div class="status-msg ok">{saveTestResult}</div>
      {/if}
    </div>

    <!-- Step 4: Rollback test -->
    <div class="section">
      <h3 class="section-title">4. ROLLBACK ENGINE TEST</h3>
      <p class="section-desc">Starts the full rollback loop against a single Dolphin. Tests frame detection, input reading, save states, and the rollback pipeline. Start a fight first!</p>
      <div class="button-row">
        {#if !rollbackRunning}
          <button class="btn-primary btn-big" onclick={startRollbackTest} disabled={!debugResult?.has_valid_players}>
            START ROLLBACK TEST
          </button>
        {:else}
          <button class="btn-danger btn-big" onclick={stopRollbackTest}>STOP ROLLBACK</button>
        {/if}
      </div>

      {#if !debugResult?.has_valid_players && !rollbackRunning}
        <p class="hint">Start a fight in Dolphin and click SCAN first</p>
      {/if}

      {#if rollbackStats}
        <div class="stats-grid">
          <div class="stat">
            <span class="stat-label">FRAME</span>
            <span class="stat-value">{rollbackStats.current_frame}</span>
          </div>
          <div class="stat">
            <span class="stat-label">SAVE STATE</span>
            <span class="stat-value">{rollbackStats.save_state_ms.toFixed(1)}ms</span>
          </div>
          <div class="stat">
            <span class="stat-label">LOAD STATE</span>
            <span class="stat-value">{rollbackStats.load_state_ms.toFixed(1)}ms</span>
          </div>
          <div class="stat">
            <span class="stat-label">ROLLBACKS</span>
            <span class="stat-value">{rollbackStats.rollback_count}</span>
          </div>
          <div class="stat">
            <span class="stat-label">AVG ROLLBACK</span>
            <span class="stat-value">{rollbackStats.avg_rollback_ms.toFixed(1)}ms</span>
          </div>
          <div class="stat">
            <span class="stat-label">MAX ROLLBACK</span>
            <span class="stat-value">{rollbackStats.max_rollback_ms.toFixed(1)}ms</span>
          </div>
          <div class="stat">
            <span class="stat-label">PREDICTION</span>
            <span class="stat-value">{rollbackStats.prediction_success_rate.toFixed(0)}%</span>
          </div>
          <div class="stat">
            <span class="stat-label">PING</span>
            <span class="stat-value">{rollbackStats.ping_ms.toFixed(0)}ms</span>
          </div>
          <div class="stat" class:desync={rollbackStats.desync_detected}>
            <span class="stat-label">DESYNC</span>
            <span class="stat-value">{rollbackStats.desync_detected ? `FRAME ${rollbackStats.desync_frame}` : "NONE"}</span>
          </div>
        </div>
      {/if}
    </div>
  {/if}

  <!-- Log -->
  <div class="section log-section">
    <h3 class="section-title">LOG</h3>
    <div class="log-output">
      {#each testLog as line}
        <div class="log-line">{line}</div>
      {/each}
      {#if testLog.length === 0}
        <div class="log-line muted">Attach to Dolphin to begin.</div>
      {/if}
    </div>
  </div>
</div>

<style>
  .test-page { padding: 32px 40px; padding-bottom: 40px; }
  .page-title { font-family: 'Orbitron', monospace; font-size: 24px; font-weight: 700; letter-spacing: 3px; }
  .page-desc { color: var(--text-secondary); margin-top: 4px; font-size: 14px; }

  .section { background: var(--bg-card); border: 1px solid var(--border); border-radius: 12px; padding: 20px; margin-top: 20px; }
  .section-title { font-family: 'Orbitron', monospace; font-size: 11px; font-weight: 700; letter-spacing: 2px; color: var(--wind-cyan); margin-bottom: 12px; }
  .section-desc { font-size: 12px; color: var(--text-muted); margin-bottom: 12px; }

  .button-row { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; }

  .btn-primary { padding: 8px 20px; background: linear-gradient(135deg, var(--accent-primary), var(--wind-cyan)); color: white; font-family: 'Orbitron', monospace; font-size: 11px; font-weight: 700; letter-spacing: 1px; border-radius: 6px; transition: all 0.2s ease; }
  .btn-primary:hover { box-shadow: 0 0 16px rgba(34, 211, 238, 0.3); transform: translateY(-1px); }
  .btn-primary:disabled { opacity: 0.5; cursor: not-allowed; transform: none; }
  .btn-big { padding: 12px 32px; font-size: 13px; letter-spacing: 2px; }

  .btn-danger { padding: 8px 16px; background: rgba(239, 68, 68, 0.15); color: #ef4444; font-family: 'Orbitron', monospace; font-size: 11px; font-weight: 700; letter-spacing: 1px; border: 1px solid rgba(239, 68, 68, 0.3); border-radius: 6px; }
  .btn-danger:hover { background: rgba(239, 68, 68, 0.25); }

  .btn-secondary { padding: 8px 16px; background: var(--bg-primary); color: var(--text-secondary); font-family: 'Orbitron', monospace; font-size: 11px; font-weight: 600; letter-spacing: 1px; border: 1px solid var(--border); border-radius: 6px; }
  .btn-secondary:hover { color: var(--text-primary); border-color: var(--wind-cyan); }

  .dolphin-found { font-family: 'Orbitron', monospace; font-size: 13px; font-weight: 700; letter-spacing: 2px; color: #22c55e; text-shadow: 0 0 8px rgba(34, 197, 94, 0.4); }

  .status-msg { margin-top: 8px; padding: 8px 12px; border-radius: 6px; font-size: 12px; font-family: monospace; }
  .status-msg.ok { background: rgba(34, 197, 94, 0.1); border: 1px solid rgba(34, 197, 94, 0.2); color: #22c55e; }
  .status-msg.err { background: rgba(239, 68, 68, 0.1); border: 1px solid rgba(239, 68, 68, 0.2); color: #ef4444; }

  .debug-status { display: flex; gap: 10px; margin-top: 16px; flex-wrap: wrap; }
  .status-pill { padding: 6px 14px; border-radius: 20px; font-family: 'Orbitron', monospace; font-size: 10px; font-weight: 700; letter-spacing: 1px; }
  .status-pill.green { background: rgba(34, 197, 94, 0.15); border: 1px solid rgba(34, 197, 94, 0.3); color: #22c55e; }
  .status-pill.red { background: rgba(239, 68, 68, 0.15); border: 1px solid rgba(239, 68, 68, 0.3); color: #ef4444; }
  .status-pill.neutral { background: rgba(34, 211, 238, 0.1); border: 1px solid rgba(34, 211, 238, 0.2); color: var(--wind-cyan); }

  .hint { font-size: 12px; color: var(--text-muted); margin-top: 8px; font-style: italic; }

  .stats-grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 12px; margin-top: 16px; }
  .stat { background: rgba(0, 0, 0, 0.3); border: 1px solid var(--border); border-radius: 8px; padding: 12px; text-align: center; }
  .stat-label { display: block; font-family: 'Orbitron', monospace; font-size: 9px; font-weight: 700; letter-spacing: 1px; color: var(--text-muted); margin-bottom: 4px; }
  .stat-value { display: block; font-family: 'Orbitron', monospace; font-size: 16px; font-weight: 700; color: var(--wind-cyan); }
  .stat.desync { border-color: rgba(239, 68, 68, 0.5); }
  .stat.desync .stat-value { color: #ef4444; }

  .log-section { max-height: 300px; }
  .log-output { background: var(--bg-primary); border: 1px solid var(--border); border-radius: 6px; padding: 10px; max-height: 200px; overflow-y: auto; font-family: monospace; font-size: 11px; }
  .log-line { color: var(--text-secondary); line-height: 1.6; }
  .log-line.muted { color: var(--text-muted); font-style: italic; }
</style>
