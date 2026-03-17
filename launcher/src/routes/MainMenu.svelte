<script lang="ts">
  interface Props {
    onNavigate: (route: string) => void;
  }

  let { onNavigate }: Props = $props();
</script>

<div class="main-menu">
  <div class="hero">
    <div class="hero-bg"></div>
    <div class="hero-content">
      <h1 class="title">HOWLING WIND</h1>
      <p class="subtitle">Rollback Netplay for Naruto GNT4</p>
      <p class="tagline">The storm has arrived. Fight with zero delay.</p>
    </div>
  </div>

  <div class="action-cards">
    <button class="action-card primary" onclick={() => onNavigate("play")}>
      <div class="card-icon">&#9889;</div>
      <div class="card-info">
        <h3>PLAY ONLINE</h3>
        <p>Host or join a lobby with rollback netcode</p>
      </div>
      <div class="card-arrow">&rarr;</div>
    </button>

    <button class="action-card" onclick={() => onNavigate("practice")}>
      <div class="card-icon">&#9876;</div>
      <div class="card-info">
        <h3>PRACTICE</h3>
        <p>Launch GNT4 for local play and training</p>
      </div>
      <div class="card-arrow">&rarr;</div>
    </button>

    <button class="action-card" onclick={() => onNavigate("settings")}>
      <div class="card-icon">&#9881;</div>
      <div class="card-info">
        <h3>SETTINGS</h3>
        <p>Configure paths, controls, and network</p>
      </div>
      <div class="card-arrow">&rarr;</div>
    </button>
  </div>

  <div class="stats-bar">
    <div class="stat">
      <span class="stat-value">0</span>
      <span class="stat-label">Players Online</span>
    </div>
    <div class="stat">
      <span class="stat-value">--</span>
      <span class="stat-label">Your Ping</span>
    </div>
    <div class="stat">
      <span class="stat-value">0.1.0</span>
      <span class="stat-label">Version</span>
    </div>
  </div>
</div>

<style>
  .main-menu {
    display: flex;
    flex-direction: column;
    height: 100%;
    padding: 0;
  }

  .hero {
    position: relative;
    padding: 48px 40px 32px;
    overflow: hidden;
  }

  .hero-bg {
    position: absolute;
    inset: 0;
    background:
      radial-gradient(ellipse at 30% 20%, rgba(34, 211, 238, 0.08) 0%, transparent 60%),
      radial-gradient(ellipse at 70% 80%, rgba(59, 130, 246, 0.06) 0%, transparent 50%);
  }

  .hero-content {
    position: relative;
    z-index: 1;
  }

  .title {
    font-family: 'Orbitron', monospace;
    font-size: 42px;
    font-weight: 900;
    letter-spacing: 6px;
    background: linear-gradient(135deg, var(--wind-cyan), var(--accent-primary), var(--accent-secondary));
    -webkit-background-clip: text;
    -webkit-text-fill-color: transparent;
    background-clip: text;
    text-shadow: none;
    filter: drop-shadow(0 0 20px rgba(34, 211, 238, 0.3));
  }

  .subtitle {
    font-size: 18px;
    font-weight: 500;
    color: var(--text-secondary);
    margin-top: 8px;
    letter-spacing: 1px;
  }

  .tagline {
    font-size: 14px;
    color: var(--text-muted);
    margin-top: 4px;
    font-style: italic;
  }

  .action-cards {
    flex: 1;
    padding: 8px 40px 24px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .action-card {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 20px 24px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 12px;
    color: var(--text-primary);
    transition: all 0.2s ease;
    text-align: left;
  }

  .action-card:hover {
    background: var(--bg-card-hover);
    border-color: rgba(34, 211, 238, 0.3);
    box-shadow: var(--shadow-glow);
    transform: translateX(4px);
  }

  .action-card.primary {
    border-color: rgba(34, 211, 238, 0.2);
    background: linear-gradient(135deg, rgba(34, 211, 238, 0.08), rgba(59, 130, 246, 0.05));
  }

  .action-card.primary:hover {
    border-color: var(--wind-cyan);
    box-shadow: 0 0 30px rgba(34, 211, 238, 0.2);
  }

  .card-icon {
    font-size: 28px;
    width: 48px;
    height: 48px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(34, 211, 238, 0.1);
    border-radius: 10px;
    flex-shrink: 0;
  }

  .card-info {
    flex: 1;
  }

  .card-info h3 {
    font-family: 'Orbitron', monospace;
    font-size: 14px;
    font-weight: 700;
    letter-spacing: 2px;
    color: var(--text-primary);
  }

  .card-info p {
    font-size: 13px;
    color: var(--text-secondary);
    margin-top: 2px;
  }

  .card-arrow {
    font-size: 20px;
    color: var(--text-muted);
    transition: all 0.2s ease;
  }

  .action-card:hover .card-arrow {
    color: var(--wind-cyan);
    transform: translateX(4px);
  }

  .stats-bar {
    display: flex;
    gap: 32px;
    padding: 16px 40px;
    border-top: 1px solid var(--border);
    background: var(--bg-secondary);
  }

  .stat {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .stat-value {
    font-family: 'Orbitron', monospace;
    font-size: 14px;
    font-weight: 700;
    color: var(--wind-cyan);
  }

  .stat-label {
    font-size: 11px;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 1px;
  }
</style>
