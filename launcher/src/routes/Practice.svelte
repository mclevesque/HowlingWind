<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  interface GameInfo {
    id: string;
    name: string;
    game_id: string;
    iso_path: string;
  }

  let games = $state<GameInfo[]>([]);
  let selectedGame = $state<GameInfo | null>(null);
  let launching = $state(false);
  let errorMsg = $state("");
  let scanning = $state(true);

  // Scan for games on mount
  (async () => {
    try {
      const found: GameInfo[] = await invoke("scan_games");
      games = found;
      if (found.length > 0) {
        // Default to GNT4 if available, otherwise first game
        selectedGame = found.find(g => g.id === "gnt4") || found[0];
      }
    } catch (e: any) {
      errorMsg = "Failed to scan games: " + e.toString();
    } finally {
      scanning = false;
    }
  })();

  async function launchGame() {
    if (!selectedGame) return;
    launching = true;
    errorMsg = "";
    try {
      await invoke("launch_dolphin", {
        mode: "practice",
        isoOverride: selectedGame.iso_path,
      });
    } catch (e: any) {
      errorMsg = e.toString();
    } finally {
      launching = false;
    }
  }
</script>

<div class="practice">
  <h2 class="page-title">PRACTICE</h2>
  <p class="page-desc">Launch a game for local play and training</p>

  <div class="launch-section">
    {#if scanning}
      <div class="scanning">Scanning for games...</div>
    {:else if games.length === 0}
      <div class="no-games">
        <p>No games found. Place ISO files in the <code>games/</code> folder.</p>
      </div>
    {:else}
      <div class="game-selector">
        {#each games as game}
          <button
            class="game-card"
            class:selected={selectedGame?.id === game.id}
            onclick={() => selectedGame = game}
          >
            <div class="game-icon">{game.id === "gntsp" ? "⚡" : "🌀"}</div>
            <div class="game-info">
              <h3>{game.name}</h3>
              <p>GameCube &middot; {game.game_id}</p>
            </div>
          </button>
        {/each}
      </div>

      <button
        class="btn-launch"
        onclick={launchGame}
        disabled={launching || !selectedGame}
      >
        {#if launching}
          <div class="spinner"></div>
          Launching...
        {:else}
          &#9654; LAUNCH {selectedGame?.name?.toUpperCase() || "GAME"}
        {/if}
      </button>
    {/if}

    {#if errorMsg}
      <div class="error-msg">
        <strong>Error:</strong> {errorMsg}
        <p class="error-hint">Make sure Dolphin path is set in Settings and ISO files are in the games folder.</p>
      </div>
    {/if}
  </div>

  <div class="tips">
    <h3 class="section-title">QUICK TIPS</h3>
    <ul>
      <li>Drop ISOs into the <strong>games/</strong> folder and they'll appear here</li>
      <li>Gecko codes (unlock all, skip intro) are applied automatically per game</li>
      <li>Both GNT4 and GNT Special are supported for rollback netplay</li>
    </ul>
  </div>
</div>

<style>
  .practice {
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

  .launch-section {
    margin-top: 32px;
    display: flex;
    flex-direction: column;
    gap: 20px;
  }

  .game-selector {
    display: flex;
    gap: 12px;
  }

  .game-card {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 20px 24px;
    background: var(--bg-card);
    border: 2px solid var(--border);
    border-radius: 12px;
    cursor: pointer;
    transition: all 0.2s ease;
    text-align: left;
    color: var(--text-primary);
  }

  .game-card:hover {
    border-color: rgba(34, 211, 238, 0.3);
  }

  .game-card.selected {
    border-color: var(--wind-cyan);
    background: rgba(34, 211, 238, 0.05);
    box-shadow: 0 0 20px rgba(34, 211, 238, 0.1);
  }

  .game-icon {
    font-size: 36px;
    width: 56px;
    height: 56px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(34, 211, 238, 0.1);
    border-radius: 10px;
  }

  .game-info h3 {
    font-size: 16px;
    font-weight: 600;
  }

  .game-info p {
    font-size: 13px;
    color: var(--text-secondary);
  }

  .btn-launch {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 10px;
    padding: 16px 32px;
    background: linear-gradient(135deg, var(--accent-primary), var(--wind-cyan));
    color: white;
    font-family: 'Orbitron', monospace;
    font-size: 14px;
    font-weight: 700;
    letter-spacing: 3px;
    border-radius: 12px;
    transition: all 0.2s ease;
  }

  .btn-launch:hover:not(:disabled) {
    box-shadow: 0 0 30px rgba(34, 211, 238, 0.3);
    transform: translateY(-2px);
  }

  .btn-launch:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .spinner {
    width: 16px;
    height: 16px;
    border: 2px solid rgba(255,255,255,0.3);
    border-top-color: white;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .scanning {
    color: var(--text-secondary);
    font-size: 14px;
    padding: 20px;
    text-align: center;
  }

  .no-games {
    padding: 24px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 12px;
    text-align: center;
    color: var(--text-secondary);
  }

  .no-games code {
    color: var(--wind-cyan);
    background: rgba(34, 211, 238, 0.1);
    padding: 2px 6px;
    border-radius: 4px;
  }

  .error-msg {
    padding: 12px 16px;
    background: rgba(239, 68, 68, 0.1);
    border: 1px solid rgba(239, 68, 68, 0.3);
    border-radius: 8px;
    color: var(--danger);
    font-size: 13px;
  }

  .error-hint {
    color: var(--text-secondary);
    margin-top: 4px;
    font-size: 12px;
  }

  .tips {
    margin-top: 40px;
    padding: 24px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 12px;
  }

  .section-title {
    font-family: 'Orbitron', monospace;
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 2px;
    color: var(--text-secondary);
    margin-bottom: 12px;
  }

  .tips ul {
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .tips li {
    font-size: 13px;
    color: var(--text-secondary);
    padding-left: 16px;
    position: relative;
  }

  .tips li::before {
    content: ">";
    position: absolute;
    left: 0;
    color: var(--wind-cyan);
    font-family: 'Orbitron', monospace;
    font-size: 11px;
  }

  .tips li strong {
    color: var(--text-primary);
  }
</style>
