<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { open } from "@tauri-apps/plugin-dialog";
  import ControllerMapper from "../lib/ControllerMapper.svelte";

  let dolphinPath = $state("");
  let isoPath = $state("");
  let playerName = $state("Player");
  let inputDelay = $state(2);
  let maxRollback = $state(7);
  let resolution = $state(2);
  let saved = $state(false);
  let showAdvanced = $state(false);
  let dolphinDetected = $state(false);
  let isoDetected = $state(false);

  const resolutionOptions = [
    { value: 1, label: "Native (480p)", desc: "Original GameCube" },
    { value: 2, label: "2x (960p)", desc: "Recommended" },
    { value: 3, label: "3x (1440p)", desc: "High quality" },
    { value: 4, label: "4x (2160p/4K)", desc: "Best quality" },
  ];

  // Load settings on mount
  (async () => {
    try {
      const s: any = await invoke("get_settings");
      dolphinPath = s.dolphin_path || "";
      isoPath = s.iso_path || "";
      playerName = s.player_name || "Player";
      inputDelay = s.input_delay ?? 2;
      maxRollback = s.max_rollback ?? 7;
      resolution = s.resolution ?? 2;
      dolphinDetected = dolphinPath.length > 0;
      isoDetected = isoPath.length > 0;
    } catch {}
  })();

  async function browseDolphin() {
    try {
      const selected = await open({
        filters: [{ name: "Dolphin", extensions: ["exe"] }],
        multiple: false,
      });
      if (selected) dolphinPath = selected as string;
    } catch {}
  }

  async function browseISO() {
    try {
      const selected = await open({
        filters: [{ name: "GameCube ISO", extensions: ["iso", "gcm", "ciso"] }],
        multiple: false,
      });
      if (selected) isoPath = selected as string;
    } catch {}
  }

  async function saveSettings() {
    try {
      await invoke("save_settings", {
        settings: {
          dolphin_path: dolphinPath,
          iso_path: isoPath,
          player_name: playerName,
          input_delay: inputDelay,
          max_rollback: maxRollback,
          resolution: resolution,
        }
      });
      saved = true;
      setTimeout(() => saved = false, 2000);
    } catch (e: any) {
      alert("Failed to save: " + e.toString());
    }
  }
</script>

<div class="settings">
  <h2 class="page-title">SETTINGS</h2>
  <p class="page-desc">Configure HowlingWind to your preferences</p>

  <div class="settings-groups">
    <!-- Auto-detected status -->
    <div class="settings-group">
      <h3 class="group-title">GAME</h3>

      <div class="status-row">
        <div class="status-item" class:ok={dolphinDetected} class:missing={!dolphinDetected}>
          <span class="status-icon">{dolphinDetected ? "OK" : "!"}</span>
          <span class="status-text">
            {dolphinDetected ? "Dolphin detected" : "Dolphin not found"}
          </span>
        </div>
        <div class="status-item" class:ok={isoDetected} class:missing={!isoDetected}>
          <span class="status-icon">{isoDetected ? "OK" : "!"}</span>
          <span class="status-text">
            {isoDetected ? "GNT4 ISO detected" : "GNT4 ISO not found"}
          </span>
        </div>
      </div>

      {#if !dolphinDetected || !isoDetected}
        <p class="path-note">
          Place Dolphin in the <code>dolphin/Dolphin-x64/</code> folder and your ISO in the <code>games/</code> folder next to HowlingWind, or set paths below.
        </p>
      {/if}

      <button class="btn-toggle" onclick={() => showAdvanced = !showAdvanced}>
        {showAdvanced ? "Hide Paths" : "Edit Paths"}
      </button>

      {#if showAdvanced}
        <div class="paths-section">
          <div class="setting-row">
            <label>Dolphin Executable</label>
            <div class="path-input">
              <input type="text" bind:value={dolphinPath} placeholder="Auto-detected..." />
              <button class="btn-browse" onclick={browseDolphin}>Browse</button>
            </div>
          </div>

          <div class="setting-row">
            <label>GNT4 ISO</label>
            <div class="path-input">
              <input type="text" bind:value={isoPath} placeholder="Auto-detected..." />
              <button class="btn-browse" onclick={browseISO}>Browse</button>
            </div>
          </div>
        </div>
      {/if}
    </div>

    <div class="settings-group">
      <h3 class="group-title">PLAYER</h3>

      <div class="setting-row">
        <label>Display Name</label>
        <input type="text" bind:value={playerName} maxlength="20" class="text-input" />
      </div>
    </div>

    <div class="settings-group">
      <h3 class="group-title">NETWORK</h3>

      <div class="setting-row">
        <label>
          Input Delay (frames)
          <span class="setting-hint">Lower = more responsive, but more rollbacks</span>
        </label>
        <div class="slider-group">
          <input type="range" min="0" max="5" bind:value={inputDelay} class="slider" />
          <span class="slider-value">{inputDelay}</span>
        </div>
      </div>

      <div class="setting-row">
        <label>
          Max Rollback Frames
          <span class="setting-hint">Higher = tolerates more latency, but may look choppy</span>
        </label>
        <div class="slider-group">
          <input type="range" min="1" max="10" bind:value={maxRollback} class="slider" />
          <span class="slider-value">{maxRollback}</span>
        </div>
      </div>
    </div>

    <div class="settings-group">
      <h3 class="group-title">GRAPHICS</h3>

      <div class="setting-row">
        <label>
          Internal Resolution
          <span class="setting-hint">Higher = sharper visuals, requires more GPU power</span>
        </label>
        <div class="resolution-options">
          {#each resolutionOptions as opt}
            <button
              class="resolution-btn"
              class:active={resolution === opt.value}
              onclick={() => resolution = opt.value}
            >
              <span class="res-label">{opt.label}</span>
              <span class="res-desc">{opt.desc}</span>
            </button>
          {/each}
        </div>
      </div>
    </div>

    <ControllerMapper />
  </div>

  <div class="save-bar">
    <button class="btn-save" onclick={saveSettings}>
      {saved ? "Saved!" : "Save Settings"}
    </button>
  </div>
</div>

<style>
  .settings {
    padding: 32px 40px;
    padding-bottom: 100px;
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

  .settings-groups {
    margin-top: 32px;
    display: flex;
    flex-direction: column;
    gap: 24px;
  }

  .settings-group {
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 24px;
  }

  .group-title {
    font-family: 'Orbitron', monospace;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 2px;
    color: var(--wind-cyan);
    margin-bottom: 20px;
  }

  .status-row {
    display: flex;
    gap: 16px;
    margin-bottom: 16px;
  }

  .status-item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 14px;
    border-radius: 8px;
    font-size: 13px;
    font-weight: 600;
  }

  .status-item.ok {
    background: rgba(34, 197, 94, 0.1);
    border: 1px solid rgba(34, 197, 94, 0.2);
    color: #22c55e;
  }

  .status-item.missing {
    background: rgba(239, 68, 68, 0.1);
    border: 1px solid rgba(239, 68, 68, 0.2);
    color: #ef4444;
  }

  .status-icon {
    font-family: 'Orbitron', monospace;
    font-size: 10px;
    font-weight: 700;
  }

  .path-note {
    font-size: 12px;
    color: var(--text-muted);
    margin-bottom: 12px;
    line-height: 1.5;
  }

  .path-note code {
    background: var(--bg-primary);
    padding: 2px 6px;
    border-radius: 4px;
    font-size: 11px;
    color: var(--wind-cyan);
  }

  .btn-toggle {
    padding: 6px 16px;
    background: var(--bg-primary);
    color: var(--text-secondary);
    border: 1px solid var(--border);
    border-radius: 6px;
    font-size: 12px;
    font-weight: 600;
    transition: all 0.15s ease;
  }

  .btn-toggle:hover {
    color: var(--text-primary);
    border-color: var(--text-muted);
  }

  .paths-section {
    margin-top: 16px;
  }

  .setting-row {
    margin-bottom: 16px;
  }

  .setting-row:last-child {
    margin-bottom: 0;
  }

  .setting-row label {
    display: block;
    font-size: 13px;
    font-weight: 600;
    color: var(--text-primary);
    margin-bottom: 6px;
  }

  .setting-hint {
    display: block;
    font-size: 11px;
    font-weight: 400;
    color: var(--text-muted);
    margin-top: 2px;
  }

  .path-input {
    display: flex;
    gap: 8px;
  }

  .path-input input, .text-input {
    flex: 1;
    padding: 8px 12px;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-primary);
    font-size: 13px;
  }

  .path-input input:focus, .text-input:focus {
    border-color: var(--wind-cyan);
  }

  .path-input input::placeholder {
    color: var(--text-muted);
  }

  .btn-browse {
    padding: 8px 16px;
    background: var(--bg-card-hover);
    color: var(--text-secondary);
    border: 1px solid var(--border);
    border-radius: 6px;
    font-size: 12px;
    font-weight: 600;
    transition: all 0.15s ease;
  }

  .btn-browse:hover {
    color: var(--text-primary);
    border-color: var(--accent-primary);
  }

  .slider-group {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .slider {
    flex: 1;
    -webkit-appearance: none;
    appearance: none;
    height: 4px;
    background: var(--border);
    border-radius: 2px;
    outline: none;
  }

  .slider::-webkit-slider-thumb {
    -webkit-appearance: none;
    appearance: none;
    width: 16px;
    height: 16px;
    background: var(--wind-cyan);
    border-radius: 50%;
    cursor: pointer;
    box-shadow: 0 0 6px rgba(34, 211, 238, 0.4);
  }

  .slider-value {
    font-family: 'Orbitron', monospace;
    font-size: 16px;
    font-weight: 700;
    color: var(--wind-cyan);
    min-width: 24px;
    text-align: center;
  }

  .save-bar {
    position: fixed;
    bottom: 0;
    right: 0;
    left: 220px;
    padding: 16px 40px;
    background: var(--bg-secondary);
    border-top: 1px solid var(--border);
    display: flex;
    justify-content: flex-end;
  }

  .btn-save {
    padding: 10px 32px;
    background: linear-gradient(135deg, var(--accent-primary), var(--wind-cyan));
    color: white;
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 1px;
    border-radius: 8px;
    transition: all 0.2s ease;
  }

  .btn-save:hover {
    box-shadow: 0 0 20px rgba(34, 211, 238, 0.3);
    transform: translateY(-1px);
  }

  .resolution-options {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 8px;
  }

  .resolution-btn {
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 12px 8px;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 8px;
    cursor: pointer;
    transition: all 0.15s ease;
  }

  .resolution-btn:hover {
    border-color: var(--text-muted);
  }

  .resolution-btn.active {
    background: rgba(34, 211, 238, 0.1);
    border-color: var(--wind-cyan);
    box-shadow: 0 0 8px rgba(34, 211, 238, 0.15);
  }

  .res-label {
    font-size: 12px;
    font-weight: 700;
    color: var(--text-primary);
  }

  .resolution-btn.active .res-label {
    color: var(--wind-cyan);
  }

  .res-desc {
    font-size: 10px;
    color: var(--text-muted);
    margin-top: 2px;
  }
</style>
