<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  interface Props {
    currentRoute: string;
    onNavigate: (route: string) => void;
  }

  let { currentRoute, onNavigate }: Props = $props();
  let version = $state("v0.1.0");

  // Fetch version from backend
  (async () => {
    try {
      const v = await invoke("get_app_version") as string;
      version = `v${v}`;
    } catch {}
  })();

  const navItems = [
    { id: "home", label: "HOME", icon: "&#9878;" },
    { id: "play", label: "PLAY ONLINE", icon: "&#9889;" },
    { id: "practice", label: "PRACTICE", icon: "&#9876;" },
    { id: "leaderboard", label: "RANKINGS", icon: "&#9733;" },
    { id: "settings", label: "SETTINGS", icon: "&#9881;" },
  ];
</script>

<aside class="sidebar">
  <div class="logo-section">
    <div class="logo-icon">&#127744;</div>
    <h1 class="logo-text">HOWLING<br/>WIND</h1>
    <div class="logo-divider"></div>
  </div>

  <nav class="nav">
    {#each navItems as item}
      <button
        class="nav-item"
        class:active={currentRoute === item.id}
        onclick={() => onNavigate(item.id)}
      >
        <span class="nav-icon">{@html item.icon}</span>
        <span class="nav-label">{item.label}</span>
        {#if currentRoute === item.id}
          <div class="active-indicator"></div>
        {/if}
      </button>
    {/each}
  </nav>

  <div class="sidebar-footer">
    <div class="status-dot online"></div>
    <span class="status-text">Offline</span>
    <span class="version">{version}</span>
  </div>
</aside>

<style>
  .sidebar {
    width: 220px;
    min-width: 220px;
    background: var(--bg-secondary);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    padding: 0;
  }

  .logo-section {
    padding: 24px 20px 16px;
    text-align: center;
  }

  .logo-icon {
    font-size: 36px;
    margin-bottom: 4px;
    filter: drop-shadow(0 0 8px rgba(34, 211, 238, 0.4));
  }

  .logo-text {
    font-family: 'Orbitron', monospace;
    font-size: 18px;
    font-weight: 900;
    letter-spacing: 3px;
    line-height: 1.2;
    background: linear-gradient(135deg, var(--wind-cyan), var(--accent-primary));
    -webkit-background-clip: text;
    -webkit-text-fill-color: transparent;
    background-clip: text;
  }

  .logo-divider {
    height: 1px;
    background: linear-gradient(90deg, transparent, var(--wind-cyan), transparent);
    margin-top: 16px;
    opacity: 0.4;
  }

  .nav {
    flex: 1;
    padding: 8px 12px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .nav-item {
    position: relative;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 16px;
    background: transparent;
    color: var(--text-secondary);
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 1.5px;
    border-radius: 8px;
    transition: all 0.2s ease;
    text-align: left;
  }

  .nav-item:hover {
    background: var(--bg-card);
    color: var(--text-primary);
  }

  .nav-item.active {
    background: linear-gradient(135deg, rgba(59, 130, 246, 0.15), rgba(34, 211, 238, 0.1));
    color: var(--wind-cyan);
    border: 1px solid rgba(34, 211, 238, 0.2);
  }

  .nav-icon {
    font-size: 18px;
    width: 24px;
    text-align: center;
  }

  .active-indicator {
    position: absolute;
    left: 0;
    top: 50%;
    transform: translateY(-50%);
    width: 3px;
    height: 60%;
    background: var(--wind-cyan);
    border-radius: 0 3px 3px 0;
    box-shadow: 0 0 8px var(--wind-cyan);
  }

  .sidebar-footer {
    padding: 16px 20px;
    border-top: 1px solid var(--border);
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: var(--text-muted);
  }

  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-muted);
  }

  .status-dot.online {
    background: var(--text-muted);
  }

  .version {
    margin-left: auto;
    font-family: 'Orbitron', monospace;
    font-size: 10px;
  }
</style>
