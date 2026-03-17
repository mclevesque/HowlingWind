<script lang="ts">
  import "./app.css";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";

  const appWindow = getCurrentWindow();
  import Sidebar from "./lib/Sidebar.svelte";
  import MainMenu from "./routes/MainMenu.svelte";
  import PlayOnline from "./routes/PlayOnline.svelte";
  import Settings from "./routes/Settings.svelte";
  import Practice from "./routes/Practice.svelte";
  import Leaderboard from "./routes/Leaderboard.svelte";
  import RollbackTest from "./routes/RollbackTest.svelte";
  import DebugPlay from "./routes/DebugPlay.svelte";
  import MatchResults from "./lib/MatchResults.svelte";
  import VoiceChat from "./lib/VoiceChat.svelte";
  import { initAudio, startAmbientWind, resumeAudio, onGameStart, onGameEnd, temporaryUnduck, playSfx } from "./lib/audio";
  import { recordMatchResult, calculateEloDelta, ensurePlayerRating } from "./lib/firebase";

  let currentRoute = $state("home");
  let gameRunning = $state(false);

  // Voice chat state — persists across routes and during gameplay
  let voiceChatVisible = $state(false);
  let voiceChatRoomId = $state("");
  let voiceChatPlayerId = $state("");

  // Auto-updater
  let updateAvailable = $state(false);
  let updateVersion = $state("");
  let updateUrl = $state("");
  let updateNotes = $state("");
  let updateDownloading = $state(false);
  let updatePercent = $state(0);
  let updatePhase = $state(""); // downloading | extracting | done | error
  let updateMessage = $state("");
  let updateComplete = $state(false);
  let appVersion = $state("");

  // Check for updates on launch
  (async () => {
    try {
      appVersion = await invoke("get_app_version") as string;
      const result: any = await invoke("check_for_updates");
      if (result.update_available) {
        updateAvailable = true;
        updateVersion = result.latest_version;
        updateUrl = result.download_url;
        updateNotes = result.notes;
      }
    } catch {
      // Update check failed silently — not critical
    }
  })();

  // Listen for download progress events
  listen("update-progress", (event: any) => {
    const p = event.payload;
    updatePercent = p.percent;
    updatePhase = p.phase;
    updateMessage = p.message;
    if (p.phase === "done") {
      updateComplete = true;
      updateDownloading = false;
    } else if (p.phase === "error") {
      updateDownloading = false;
    }
  });

  async function downloadAndApplyUpdate() {
    if (!updateUrl) return;
    updateDownloading = true;
    updatePercent = 0;
    updatePhase = "downloading";
    updateMessage = "Starting download...";
    try {
      await invoke("download_update", { url: updateUrl });
    } catch (e: any) {
      updateMessage = `Update failed: ${e}`;
      updatePhase = "error";
      updateDownloading = false;
    }
  }

  async function restartApp() {
    try {
      await invoke("apply_update_and_restart");
    } catch (e: any) {
      // Fallback: just close and let user reopen
      updateMessage = `Restart failed: ${e}. Please close and reopen HowlingWind manually.`;
      updatePhase = "error";
    }
  }


  // Match results overlay
  let showMatchResults = $state(false);
  let matchWinner = $state<"p1" | "p2" | "draw">("draw");
  let matchP1Name = $state("Player 1");
  let matchP2Name = $state("Player 2");
  let matchP1EloOld = $state(1200);
  let matchP2EloOld = $state(1200);
  let matchP1EloNew = $state(1200);
  let matchP2EloNew = $state(1200);
  let matchEloDelta = $state(0);

  // Match win detection polling during gameplay
  let winCheckInterval: number | null = null;

  function startWinDetection() {
    if (winCheckInterval) return;
    winCheckInterval = window.setInterval(async () => {
      try {
        const outcome: any = await invoke("dolphin_mem_check_winner");
        if (outcome.result !== "playing") {
          stopWinDetection();
          await handleMatchEnd(outcome.result);
        }
      } catch {
        // Dolphin may not be attached yet or game not loaded
      }
    }, 500); // Check every 500ms
  }

  function stopWinDetection() {
    if (winCheckInterval) {
      clearInterval(winCheckInterval);
      winCheckInterval = null;
    }
  }

  async function handleMatchEnd(result: string) {
    // Get player names from settings
    try {
      const settings: any = await invoke("get_settings");
      matchP1Name = settings.player_name || "Player 1";
    } catch {}
    matchP2Name = "Opponent"; // Will come from netplay session

    // Map result to our format
    matchWinner = result === "p1_win" ? "p1" : result === "p2_win" ? "p2" : "draw";

    // Calculate ELO (will be replaced with real Firebase data in online mode)
    matchP1EloOld = 1200;
    matchP2EloOld = 1200;
    matchEloDelta = calculateEloDelta(matchP1EloOld, matchP2EloOld);
    if (matchWinner === "p1") {
      matchP1EloNew = matchP1EloOld + matchEloDelta;
      matchP2EloNew = matchP2EloOld - matchEloDelta;
    } else if (matchWinner === "p2") {
      matchP1EloNew = matchP1EloOld - matchEloDelta;
      matchP2EloNew = matchP2EloOld + matchEloDelta;
    } else {
      matchP1EloNew = matchP1EloOld;
      matchP2EloNew = matchP2EloOld;
      matchEloDelta = 0;
    }

    // Unduck audio briefly for the results SFX
    temporaryUnduck(5000);

    showMatchResults = true;
  }

  function handleRematch() {
    showMatchResults = false;
    // Game continues running, start win detection again
    startWinDetection();
  }

  function handleExitToLobby() {
    showMatchResults = false;
    stopGame();
  }

  function navigate(route: string) {
    currentRoute = route;
  }

  // Initialize audio on first user interaction
  function handleFirstInteraction() {
    initAudio();
    resumeAudio();
    startAmbientWind();
    document.removeEventListener("click", handleFirstInteraction);
    document.removeEventListener("keydown", handleFirstInteraction);
  }
  if (typeof document !== "undefined") {
    document.addEventListener("click", handleFirstInteraction, { once: true });
    document.addEventListener("keydown", handleFirstInteraction, { once: true });
  }

  // Voice chat activation — triggered from PlayOnline when joining a room
  listen("voice-chat-start", (event: any) => {
    voiceChatRoomId = event.payload.roomId || "";
    voiceChatPlayerId = event.payload.playerId || "";
    voiceChatVisible = true;
  });
  listen("voice-chat-stop", () => {
    voiceChatVisible = false;
    voiceChatRoomId = "";
    voiceChatPlayerId = "";
  });

  listen("game-embedded", () => {
    gameRunning = true;
    onGameStart(); // Duck UI audio
    startWinDetection(); // Poll for match end
    startDolphinMonitor(); // Watch for Dolphin exit
  });

  // Monitor Dolphin process — if it exits, return to menu (don't close app)
  let dolphinMonitorInterval: number | null = null;
  function startDolphinMonitor() {
    if (dolphinMonitorInterval) return;
    dolphinMonitorInterval = window.setInterval(async () => {
      try {
        // Check if Dolphin is still running by trying to resize (will fail if dead)
        const win = getCurrentWindow();
        const size = await win.innerSize();
        await invoke("resize_embedded", { width: size.width, height: size.height });
      } catch {
        // Dolphin process died — return to menu
        stopDolphinMonitor();
        stopWinDetection();
        gameRunning = false;
        onGameEnd();
        try { await invoke("stop_dolphin"); } catch {}
      }
    }, 2000);
  }
  function stopDolphinMonitor() {
    if (dolphinMonitorInterval) {
      clearInterval(dolphinMonitorInterval);
      dolphinMonitorInterval = null;
    }
  }

  async function stopGame() {
    stopWinDetection();
    stopDolphinMonitor();
    await invoke("stop_dolphin");
    gameRunning = false;
    onGameEnd(); // Restore UI audio
  }

  async function handleResize() {
    if (gameRunning) {
      const win = getCurrentWindow();
      const size = await win.innerSize();
      await invoke("resize_embedded", { width: size.width, height: size.height });
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (gameRunning && e.key === "Escape") {
      e.preventDefault();
      stopGame();
    }
  }

  if (typeof window !== "undefined") {
    window.addEventListener("resize", handleResize);
    window.addEventListener("keydown", handleKeydown);
  }
</script>

<div class="app-container" class:game-mode={gameRunning}>
  {#if updateAvailable}
    <div class="update-banner">
      {#if updateComplete}
        <span class="update-text">Update installed!</span>
        <span class="update-notes">Restart HowlingWind to use v{updateVersion}</span>
        <button class="update-btn restart-btn" onclick={restartApp}>RESTART NOW</button>
      {:else if updateDownloading}
        <span class="update-text">
          {updatePhase === "extracting" ? "Extracting..." : "Downloading v" + updateVersion + "..."}
        </span>
        <div class="update-progress-bar">
          <div class="update-progress-fill" style="width: {updatePercent}%"></div>
        </div>
        <span class="update-percent">{Math.round(updatePercent)}%</span>
      {:else if updatePhase === "error"}
        <span class="update-text">Update failed</span>
        <span class="update-notes">{updateMessage}</span>
        <button class="update-btn" onclick={downloadAndApplyUpdate}>Retry</button>
        <button class="update-dismiss" onclick={() => updateAvailable = false}>x</button>
      {:else}
        <span class="update-text">HowlingWind v{updateVersion} available!</span>
        {#if updateNotes}<span class="update-notes">{updateNotes}</span>{/if}
        <button class="update-btn" onclick={downloadAndApplyUpdate}>Download Update</button>
        <button class="update-dismiss" onclick={() => updateAvailable = false}>x</button>
      {/if}
    </div>
  {/if}
  {#if !gameRunning}
    <Sidebar {currentRoute} onNavigate={navigate} />
    <main class="content">
      {#if currentRoute === "home"}
        <MainMenu onNavigate={navigate} />
      {:else if currentRoute === "play"}
        <PlayOnline />
      {:else if currentRoute === "practice"}
        <Practice />
      {:else if currentRoute === "leaderboard"}
        <Leaderboard />
      {:else if currentRoute === "settings"}
        <Settings />
      {:else if currentRoute === "test"}
        <RollbackTest />
      {:else if currentRoute === "debug"}
        <DebugPlay />
      {/if}
    </main>
  {:else}
    <div class="game-overlay">
      <button class="btn-exit-game" onclick={stopGame}>
        EXIT GAME
      </button>
    </div>
  {/if}
</div>

<!-- Voice Chat Widget (persists during gameplay) -->
<VoiceChat
  visible={voiceChatVisible}
  roomId={voiceChatRoomId}
  playerId={voiceChatPlayerId}
  onClose={() => voiceChatVisible = false}
/>

<!-- Match Results Overlay (renders on top of everything) -->
<MatchResults
  visible={showMatchResults}
  winner={matchWinner}
  p1Name={matchP1Name}
  p2Name={matchP2Name}
  p1EloOld={matchP1EloOld}
  p2EloOld={matchP2EloOld}
  p1EloNew={matchP1EloNew}
  p2EloNew={matchP2EloNew}
  eloDelta={matchEloDelta}
  onRematch={handleRematch}
  onExit={handleExitToLobby}
/>


<style>
  .app-container {
    display: flex;
    height: 100vh;
    width: 100vw;
    background: var(--bg-primary);
  }

  .app-container.game-mode {
    background: black;
  }

  .content {
    flex: 1;
    overflow-y: auto;
    position: relative;
  }

  .game-overlay {
    position: fixed;
    top: 8px;
    right: 8px;
    z-index: 9999;
    opacity: 0;
    transition: opacity 0.3s ease;
  }

  .game-overlay:hover {
    opacity: 1;
  }

  .btn-exit-game {
    padding: 6px 16px;
    background: rgba(0, 0, 0, 0.7);
    color: rgba(255, 255, 255, 0.8);
    font-size: 11px;
    font-family: 'Orbitron', monospace;
    letter-spacing: 1px;
    border-radius: 6px;
    backdrop-filter: blur(4px);
    border: 1px solid rgba(255, 255, 255, 0.1);
    transition: all 0.2s ease;
  }

  .btn-exit-game:hover {
    background: rgba(239, 68, 68, 0.8);
    color: white;
  }

  /* Update banner */
  .update-banner {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    z-index: 10000;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 16px;
    background: linear-gradient(135deg, rgba(34, 211, 238, 0.15), rgba(99, 102, 241, 0.15));
    border-bottom: 1px solid rgba(34, 211, 238, 0.3);
    backdrop-filter: blur(8px);
    font-size: 13px;
  }
  .update-text {
    font-family: 'Orbitron', monospace;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 1px;
    color: var(--wind-cyan, #22d3ee);
  }
  .update-notes {
    font-size: 12px;
    color: var(--text-secondary, #a0a0a0);
    flex: 1;
  }
  .update-btn {
    padding: 4px 14px;
    background: var(--wind-cyan, #22d3ee);
    color: #000;
    font-size: 11px;
    font-weight: 700;
    border-radius: 4px;
    border: none;
    cursor: pointer;
    font-family: 'Orbitron', monospace;
    letter-spacing: 1px;
  }
  .update-btn:hover { opacity: 0.85; }
  .update-downloading {
    font-size: 11px;
    color: var(--wind-cyan, #22d3ee);
    font-weight: 600;
  }
  .update-dismiss {
    background: none;
    border: none;
    color: var(--text-muted, #666);
    font-size: 14px;
    cursor: pointer;
    padding: 0 4px;
  }
  .update-dismiss:hover { color: var(--text-primary, #fff); }

  /* Progress bar */
  .update-progress-bar {
    flex: 1;
    height: 8px;
    background: rgba(255, 255, 255, 0.1);
    border-radius: 4px;
    overflow: hidden;
    min-width: 120px;
  }
  .update-progress-fill {
    height: 100%;
    background: linear-gradient(90deg, var(--wind-cyan, #22d3ee), #6366f1);
    border-radius: 4px;
    transition: width 0.3s ease;
  }
  .update-percent {
    font-family: 'Orbitron', monospace;
    font-size: 11px;
    color: var(--wind-cyan, #22d3ee);
    font-weight: 700;
    min-width: 40px;
    text-align: right;
  }
  .restart-btn {
    animation: pulse-glow 1.5s ease-in-out infinite;
  }
  @keyframes pulse-glow {
    0%, 100% { box-shadow: 0 0 4px rgba(34, 211, 238, 0.3); }
    50% { box-shadow: 0 0 12px rgba(34, 211, 238, 0.6); }
  }


</style>
