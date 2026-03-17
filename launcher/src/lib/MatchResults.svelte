<script lang="ts">
  import { playSfx } from "./audio";

  interface Props {
    visible: boolean;
    winner: "p1" | "p2" | "draw";
    p1Name: string;
    p2Name: string;
    p1EloOld: number;
    p2EloOld: number;
    p1EloNew: number;
    p2EloNew: number;
    eloDelta: number;
    onRematch: () => void;
    onExit: () => void;
  }

  let {
    visible,
    winner,
    p1Name,
    p2Name,
    p1EloOld,
    p2EloOld,
    p1EloNew,
    p2EloNew,
    eloDelta,
    onRematch,
    onExit,
  }: Props = $props();

  let showElo = $state(false);
  let animatedP1Elo = $state(0);
  let animatedP2Elo = $state(0);

  $effect(() => {
    if (visible) {
      animatedP1Elo = p1EloOld;
      animatedP2Elo = p2EloOld;

      // Play match end sound
      playSfx("match_end");

      // Animate ELO change after a beat
      setTimeout(() => {
        showElo = true;
        animateElo();

        // Play elo sound for the local player context
        if (winner === "p1") {
          playSfx("elo_up");
        } else if (winner === "p2") {
          playSfx("elo_up");
        }
      }, 1200);
    } else {
      showElo = false;
    }
  });

  function animateElo() {
    const steps = 30;
    const p1Delta = winner === "p1" ? eloDelta : winner === "p2" ? -eloDelta : 0;
    const p2Delta = winner === "p2" ? eloDelta : winner === "p1" ? -eloDelta : 0;
    let step = 0;

    function tick() {
      step++;
      const progress = step / steps;
      const eased = 1 - Math.pow(1 - progress, 3); // ease out cubic

      animatedP1Elo = Math.round(p1EloOld + p1Delta * eased);
      animatedP2Elo = Math.round(p2EloOld + p2Delta * eased);

      if (step < steps) {
        requestAnimationFrame(tick);
      }
    }
    requestAnimationFrame(tick);
  }

  function getResultText(): string {
    if (winner === "draw") return "DRAW";
    if (winner === "p1") return `${p1Name} WINS`;
    return `${p2Name} WINS`;
  }
</script>

{#if visible}
  <div class="overlay" role="dialog">
    <div class="results-card">
      <!-- Result Header -->
      <div class="result-header">
        <div class="result-flash"></div>
        <h2 class="result-text">{getResultText()}</h2>
      </div>

      <!-- Player Cards -->
      <div class="players">
        <div class="player-card" class:winner={winner === "p1"} class:loser={winner === "p2"}>
          <div class="player-tag">{winner === "p1" ? "WINNER" : winner === "p2" ? "DEFEATED" : ""}</div>
          <div class="player-name">{p1Name}</div>
          {#if showElo}
            <div class="elo-display">
              <span class="elo-value">{animatedP1Elo}</span>
              {#if winner === "p1"}
                <span class="elo-change positive">+{eloDelta}</span>
              {:else if winner === "p2"}
                <span class="elo-change negative">-{eloDelta}</span>
              {/if}
            </div>
          {/if}
        </div>

        <div class="vs-divider">VS</div>

        <div class="player-card" class:winner={winner === "p2"} class:loser={winner === "p1"}>
          <div class="player-tag">{winner === "p2" ? "WINNER" : winner === "p1" ? "DEFEATED" : ""}</div>
          <div class="player-name">{p2Name}</div>
          {#if showElo}
            <div class="elo-display">
              <span class="elo-value">{animatedP2Elo}</span>
              {#if winner === "p2"}
                <span class="elo-change positive">+{eloDelta}</span>
              {:else if winner === "p1"}
                <span class="elo-change negative">-{eloDelta}</span>
              {/if}
            </div>
          {/if}
        </div>
      </div>

      <!-- Action Buttons -->
      <div class="actions">
        <button class="btn-rematch" onclick={onRematch}>
          REMATCH
        </button>
        <button class="btn-exit" onclick={onExit}>
          BACK TO LOBBY
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.85);
    backdrop-filter: blur(8px);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 10000;
    animation: fadeIn 0.4s ease;
  }

  @keyframes fadeIn {
    from { opacity: 0; }
    to { opacity: 1; }
  }

  .results-card {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 16px;
    padding: 40px 48px;
    min-width: 500px;
    max-width: 600px;
    animation: slideUp 0.5s cubic-bezier(0.16, 1, 0.3, 1);
  }

  @keyframes slideUp {
    from {
      opacity: 0;
      transform: translateY(30px) scale(0.95);
    }
    to {
      opacity: 1;
      transform: translateY(0) scale(1);
    }
  }

  .result-header {
    text-align: center;
    margin-bottom: 32px;
    position: relative;
  }

  .result-flash {
    position: absolute;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: 200px;
    height: 200px;
    background: radial-gradient(circle, rgba(34, 211, 238, 0.15), transparent 70%);
    border-radius: 50%;
    animation: pulse 2s ease infinite;
  }

  @keyframes pulse {
    0%, 100% { transform: translate(-50%, -50%) scale(1); opacity: 0.5; }
    50% { transform: translate(-50%, -50%) scale(1.2); opacity: 0.8; }
  }

  .result-text {
    font-family: 'Orbitron', monospace;
    font-size: 28px;
    font-weight: 900;
    letter-spacing: 4px;
    background: linear-gradient(135deg, var(--wind-cyan), var(--accent-primary));
    -webkit-background-clip: text;
    -webkit-text-fill-color: transparent;
    background-clip: text;
    position: relative;
    z-index: 1;
  }

  .players {
    display: flex;
    align-items: center;
    gap: 16px;
    margin-bottom: 32px;
  }

  .player-card {
    flex: 1;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 20px;
    text-align: center;
    transition: all 0.3s ease;
  }

  .player-card.winner {
    border-color: rgba(34, 197, 94, 0.4);
    background: rgba(34, 197, 94, 0.05);
  }

  .player-card.loser {
    border-color: rgba(239, 68, 68, 0.3);
    opacity: 0.8;
  }

  .player-tag {
    font-family: 'Orbitron', monospace;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 2px;
    margin-bottom: 8px;
    height: 14px;
  }

  .winner .player-tag {
    color: #22c55e;
  }

  .loser .player-tag {
    color: #ef4444;
  }

  .player-name {
    font-size: 18px;
    font-weight: 700;
    color: var(--text-primary);
    margin-bottom: 8px;
  }

  .vs-divider {
    font-family: 'Orbitron', monospace;
    font-size: 14px;
    font-weight: 700;
    color: var(--text-muted);
    letter-spacing: 2px;
  }

  .elo-display {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    animation: fadeSlideIn 0.5s ease;
  }

  @keyframes fadeSlideIn {
    from {
      opacity: 0;
      transform: translateY(10px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  .elo-value {
    font-family: 'Orbitron', monospace;
    font-size: 20px;
    font-weight: 700;
    color: var(--wind-cyan);
  }

  .elo-change {
    font-family: 'Orbitron', monospace;
    font-size: 14px;
    font-weight: 700;
  }

  .elo-change.positive {
    color: #22c55e;
  }

  .elo-change.negative {
    color: #ef4444;
  }

  .actions {
    display: flex;
    gap: 12px;
    justify-content: center;
  }

  .btn-rematch {
    padding: 12px 40px;
    background: linear-gradient(135deg, var(--accent-primary), var(--wind-cyan));
    color: white;
    font-family: 'Orbitron', monospace;
    font-size: 14px;
    font-weight: 700;
    letter-spacing: 2px;
    border-radius: 8px;
    transition: all 0.2s ease;
  }

  .btn-rematch:hover {
    box-shadow: 0 0 24px rgba(34, 211, 238, 0.4);
    transform: translateY(-2px);
  }

  .btn-exit {
    padding: 12px 32px;
    background: transparent;
    color: var(--text-secondary);
    font-family: 'Orbitron', monospace;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 1px;
    border: 1px solid var(--border);
    border-radius: 8px;
    transition: all 0.2s ease;
  }

  .btn-exit:hover {
    color: var(--text-primary);
    border-color: var(--text-muted);
  }
</style>
