<script lang="ts">
  import { onMount } from "svelte";
  import {
    initFirebase,
    getLeaderboard,
    getMatchHistory,
    onLeaderboardChanged,
    adminSetElo,
    adminResetPlayer,
    adminDeleteMatch,
    type PlayerRating,
    type MatchResult,
  } from "../lib/firebase";

  let leaderboard = $state<Array<PlayerRating & { id: string }>>([]);
  let matchHistory = $state<MatchResult[]>([]);
  let selectedGame = $state<"gnt4" | "gntsp">("gnt4");
  let isAdmin = $state(false);
  let adminMode = $state(false);
  let loading = $state(true);

  // Admin editing state
  let editingPlayer = $state<string | null>(null);
  let editEloValue = $state(1200);

  // Click-to-reveal player ID
  let revealedPlayerId = $state<string | null>(null);

  function getShortId(uuid: string): string {
    return uuid.replace(/-/g, "").substring(0, 8).toUpperCase();
  }

  // Check admin status from settings
  const ADMIN_KEY = "howlingwind_admin";

  onMount(() => {
    initFirebase();
    // Check localStorage for admin flag (you set this once)
    isAdmin = localStorage.getItem(ADMIN_KEY) === "true";
    loadData();
  });

  async function loadData() {
    loading = true;
    try {
      leaderboard = await getLeaderboard(selectedGame);
      matchHistory = await getMatchHistory(selectedGame, 20);
    } catch (e) {
      console.error("Failed to load leaderboard:", e);
    }
    loading = false;

    // Subscribe to real-time updates
    onLeaderboardChanged(selectedGame, (lb) => {
      leaderboard = lb;
    });
  }

  function switchGame(game: "gnt4" | "gntsp") {
    selectedGame = game;
    loadData();
  }

  function getRankIcon(index: number): string {
    if (index === 0) return "&#x1F947;"; // gold
    if (index === 1) return "&#x1F948;"; // silver
    if (index === 2) return "&#x1F949;"; // bronze
    return `#${index + 1}`;
  }

  function getStreakDisplay(streak: number): string {
    if (streak > 0) return `W${streak}`;
    if (streak < 0) return `L${Math.abs(streak)}`;
    return "-";
  }

  function getStreakClass(streak: number): string {
    if (streak >= 3) return "streak-hot";
    if (streak > 0) return "streak-win";
    if (streak <= -3) return "streak-cold";
    if (streak < 0) return "streak-loss";
    return "";
  }

  function getWinRate(wins: number, losses: number): string {
    const total = wins + losses;
    if (total === 0) return "0%";
    return `${Math.round((wins / total) * 100)}%`;
  }

  // Admin actions
  async function handleSetElo(playerId: string) {
    try {
      await adminSetElo(playerId, selectedGame, editEloValue);
      editingPlayer = null;
      loadData();
    } catch (e: any) {
      alert("Failed: " + e.message);
    }
  }

  async function handleResetPlayer(playerId: string, name: string) {
    if (!confirm(`Reset ${name}'s stats to default? This cannot be undone.`)) return;
    try {
      await adminResetPlayer(playerId, selectedGame);
      loadData();
    } catch (e: any) {
      alert("Failed: " + e.message);
    }
  }

  function enableAdmin() {
    const code = prompt("Enter admin code:");
    if (code === "HowlingWindAdmin2026") {
      localStorage.setItem(ADMIN_KEY, "true");
      isAdmin = true;
      adminMode = true;
    } else {
      alert("Invalid code.");
    }
  }
</script>

<div class="leaderboard-page">
  <header class="page-header">
    <h2>LEADERBOARD</h2>
    <div class="header-controls">
      <div class="game-tabs">
        <button
          class="game-tab"
          class:active={selectedGame === "gnt4"}
          onclick={() => switchGame("gnt4")}
        >GNT4</button>
        <button
          class="game-tab"
          class:active={selectedGame === "gntsp"}
          onclick={() => switchGame("gntsp")}
        >GNTSP</button>
      </div>
      {#if isAdmin}
        <button
          class="admin-toggle"
          class:active={adminMode}
          onclick={() => (adminMode = !adminMode)}
        >ADMIN</button>
      {:else}
        <button class="admin-toggle hidden-admin" onclick={enableAdmin}>
          &#9881;
        </button>
      {/if}
    </div>
  </header>

  {#if loading}
    <div class="loading">Loading rankings...</div>
  {:else}

  <!-- Rankings Table -->
  <section class="rankings-section">
    <div class="rankings-table">
      <div class="table-header">
        <span class="col-rank">RANK</span>
        <span class="col-name">PLAYER</span>
        <span class="col-elo">ELO</span>
        <span class="col-record">W / L</span>
        <span class="col-winrate">WIN%</span>
        <span class="col-streak">STREAK</span>
        <span class="col-peak">PEAK</span>
        {#if adminMode}<span class="col-admin">ADMIN</span>{/if}
      </div>

      {#if leaderboard.length === 0}
        <div class="empty-state">
          <p>No ranked players yet.</p>
          <p class="subtext">Play a ranked match to appear here!</p>
        </div>
      {/if}

      {#each leaderboard as player, i}
        <div class="table-row" class:top3={i < 3}>
          <span class="col-rank rank-cell">
            {#if i < 3}
              <span class="rank-icon">{@html getRankIcon(i)}</span>
            {:else}
              <span class="rank-number">#{i + 1}</span>
            {/if}
          </span>
          <span
            class="col-name player-name clickable"
            onclick={() => revealedPlayerId = revealedPlayerId === player.id ? null : player.id}
            title="Click to reveal player ID"
          >
            {#if revealedPlayerId === player.id}
              <span class="player-short-id">#{getShortId(player.id)}</span>
            {:else}
              {player.name}
            {/if}
          </span>
          <span class="col-elo elo-value">{player.elo}</span>
          <span class="col-record">{player.wins} / {player.losses}</span>
          <span class="col-winrate">{getWinRate(player.wins, player.losses)}</span>
          <span class="col-streak {getStreakClass(player.streak)}">
            {getStreakDisplay(player.streak)}
          </span>
          <span class="col-peak peak-value">{player.peakElo}</span>
          {#if adminMode}
            <span class="col-admin admin-actions">
              {#if editingPlayer === player.id}
                <input
                  type="number"
                  class="elo-input"
                  bind:value={editEloValue}
                  onkeydown={(e) => e.key === "Enter" && handleSetElo(player.id)}
                />
                <button class="btn-sm btn-save" onclick={() => handleSetElo(player.id)}>SET</button>
                <button class="btn-sm btn-cancel" onclick={() => (editingPlayer = null)}>X</button>
              {:else}
                <button
                  class="btn-sm btn-edit"
                  onclick={() => { editingPlayer = player.id; editEloValue = player.elo; }}
                >ELO</button>
                <button
                  class="btn-sm btn-reset"
                  onclick={() => handleResetPlayer(player.id, player.name)}
                >RESET</button>
              {/if}
            </span>
          {/if}
        </div>
      {/each}
    </div>
  </section>

  <!-- Recent Matches -->
  <section class="history-section">
    <h3>RECENT MATCHES</h3>
    <div class="match-list">
      {#if matchHistory.length === 0}
        <div class="empty-state">No matches recorded yet.</div>
      {/if}
      {#each matchHistory as match}
        <div class="match-card">
          <span class="winner">{match.winnerName}</span>
          <span class="vs">defeated</span>
          <span class="loser">{match.loserName}</span>
          <span class="delta">+{match.eloDelta} / -{match.eloDelta}</span>
        </div>
      {/each}
    </div>
  </section>

  {/if}
</div>

<style>
  .leaderboard-page {
    padding: 32px;
    max-width: 900px;
    margin: 0 auto;
  }

  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 24px;
  }

  .page-header h2 {
    font-family: 'Orbitron', monospace;
    font-size: 22px;
    letter-spacing: 3px;
    background: linear-gradient(135deg, var(--wind-cyan), var(--accent-primary));
    -webkit-background-clip: text;
    -webkit-text-fill-color: transparent;
    background-clip: text;
  }

  .header-controls {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .game-tabs {
    display: flex;
    gap: 4px;
    background: var(--bg-secondary);
    border-radius: 8px;
    padding: 3px;
  }

  .game-tab {
    padding: 6px 16px;
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 1px;
    border-radius: 6px;
    background: transparent;
    color: var(--text-secondary);
    transition: all 0.2s;
  }

  .game-tab.active {
    background: var(--wind-cyan);
    color: var(--bg-primary);
  }

  .admin-toggle {
    padding: 6px 12px;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 1px;
    border-radius: 6px;
    background: transparent;
    color: var(--text-muted);
    border: 1px solid var(--border);
    transition: all 0.2s;
  }

  .admin-toggle.active {
    background: rgba(239, 68, 68, 0.2);
    color: #ef4444;
    border-color: rgba(239, 68, 68, 0.4);
  }

  .hidden-admin {
    opacity: 0.2;
    font-size: 14px;
  }

  .hidden-admin:hover {
    opacity: 0.6;
  }

  .loading {
    text-align: center;
    padding: 48px;
    color: var(--text-muted);
    font-family: 'Orbitron', monospace;
    font-size: 14px;
  }

  .rankings-section {
    margin-bottom: 32px;
  }

  .rankings-table {
    background: var(--bg-secondary);
    border-radius: 12px;
    border: 1px solid var(--border);
    overflow: hidden;
  }

  .table-header {
    display: grid;
    grid-template-columns: 60px 1fr 80px 80px 70px 70px 70px;
    padding: 12px 16px;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 1.5px;
    color: var(--text-muted);
    border-bottom: 1px solid var(--border);
    background: rgba(0, 0, 0, 0.2);
  }

  .table-header:has(.col-admin) {
    grid-template-columns: 60px 1fr 80px 80px 70px 70px 70px 120px;
  }

  .table-row {
    display: grid;
    grid-template-columns: 60px 1fr 80px 80px 70px 70px 70px;
    padding: 10px 16px;
    font-size: 13px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.04);
    align-items: center;
    transition: background 0.15s;
  }

  .table-row:has(.col-admin) {
    grid-template-columns: 60px 1fr 80px 80px 70px 70px 70px 120px;
  }

  .table-row:hover {
    background: rgba(34, 211, 238, 0.04);
  }

  .table-row.top3 {
    background: rgba(34, 211, 238, 0.05);
  }

  .rank-icon {
    font-size: 18px;
  }

  .rank-number {
    color: var(--text-muted);
    font-weight: 600;
  }

  .player-name {
    font-weight: 600;
    color: var(--text-primary);
  }

  .player-name.clickable {
    cursor: pointer;
    transition: color 0.15s;
  }

  .player-name.clickable:hover {
    color: var(--wind-cyan);
  }

  .player-short-id {
    font-family: 'Orbitron', monospace;
    font-size: 11px;
    color: var(--wind-cyan);
    letter-spacing: 1px;
    user-select: all;
  }

  .elo-value {
    font-family: 'Orbitron', monospace;
    font-weight: 700;
    color: var(--wind-cyan);
  }

  .peak-value {
    font-family: 'Orbitron', monospace;
    font-size: 11px;
    color: var(--text-muted);
  }

  .streak-hot {
    color: #f59e0b;
    font-weight: 700;
  }

  .streak-win {
    color: #22c55e;
  }

  .streak-cold {
    color: #ef4444;
    font-weight: 700;
  }

  .streak-loss {
    color: #f87171;
  }

  .empty-state {
    padding: 32px;
    text-align: center;
    color: var(--text-muted);
  }

  .subtext {
    font-size: 12px;
    margin-top: 4px;
    opacity: 0.6;
  }

  /* Admin controls */
  .admin-actions {
    display: flex;
    gap: 4px;
    align-items: center;
  }

  .elo-input {
    width: 60px;
    padding: 2px 4px;
    font-size: 12px;
    background: var(--bg-primary);
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: 4px;
  }

  .btn-sm {
    padding: 2px 8px;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.5px;
    border-radius: 4px;
    cursor: pointer;
  }

  .btn-edit {
    background: rgba(59, 130, 246, 0.2);
    color: #3b82f6;
    border: 1px solid rgba(59, 130, 246, 0.3);
  }

  .btn-reset {
    background: rgba(239, 68, 68, 0.2);
    color: #ef4444;
    border: 1px solid rgba(239, 68, 68, 0.3);
  }

  .btn-save {
    background: rgba(34, 197, 94, 0.2);
    color: #22c55e;
    border: 1px solid rgba(34, 197, 94, 0.3);
  }

  .btn-cancel {
    background: rgba(255, 255, 255, 0.1);
    color: var(--text-muted);
    border: 1px solid var(--border);
  }

  /* Match History */
  .history-section h3 {
    font-family: 'Orbitron', monospace;
    font-size: 14px;
    letter-spacing: 2px;
    color: var(--text-secondary);
    margin-bottom: 12px;
  }

  .match-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .match-card {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 16px;
    background: var(--bg-secondary);
    border-radius: 8px;
    border: 1px solid var(--border);
    font-size: 13px;
  }

  .winner {
    font-weight: 700;
    color: #22c55e;
  }

  .vs {
    color: var(--text-muted);
    font-size: 11px;
  }

  .loser {
    color: var(--text-secondary);
  }

  .delta {
    margin-left: auto;
    font-family: 'Orbitron', monospace;
    font-size: 11px;
    color: var(--text-muted);
  }
</style>
