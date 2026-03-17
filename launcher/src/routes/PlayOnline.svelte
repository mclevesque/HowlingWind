<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { playSfx } from "../lib/audio";
  import {
    initFirebase,
    getDb,
    setPresence,
    removePresence,
    createRoom,
    joinRoom,
    leaveRoom,
    onRoomsChanged,
    onRoomChanged,
    onPlayersChanged,
    sendSignal,
    onSignals,
    joinMatchmakingQueue,
    onMatchFound,
    leaveMatchmakingQueue,
    onFriendsChanged,
    addFriend,
    removeFriend,
    sendChallenge,
    onChallenges,
    acceptChallenge,
    declineChallenge,
    getPlayerRating,
    ensurePlayerRating,
    onFriendRequests,
    acceptFriendRequest,
    rejectFriendRequest,
    type LobbyRoom,
    type LobbyPlayer,
    type PlayerRating,
    type FriendRequest,
  } from "../lib/firebase";

  // Game mode
  let ranked = $state(true);

  // Player identity
  let playerName = $state("Player");
  let playerId = $state("");
  let shortId = $state(""); // Permanent 8-char hex ID for friend adding
  let connected = $state(false);
  let firebaseConfigured = $state(false);
  let needsNameSetup = $state(false);
  let nameInput = $state("");
  let playerRating = $state<PlayerRating | null>(null);

  // Lobby state
  let lobbyCode = $state("");
  let currentRoomId = $state("");
  let currentRoom: LobbyRoom | null = $state(null);
  let isHost = $state(false);
  let error = $state("");

  // P2P connection state
  let connectionState = $state<"idle" | "signaling" | "connecting" | "connected" | "playing">("idle");
  let directIp = $state("");
  let localPort = $state(0);
  let netplayStatus = $state("");

  // Connection quality overlay
  let showOverlay = $state(true);
  let rollbackStats = $state<{
    ping_ms: number; rollback_count: number; frames_ahead: number;
    last_rollback_depth: number; prediction_success_rate: number;
    desync_detected: boolean; current_frame: number;
  } | null>(null);
  let statsTimer: number | null = null;

  // Matchmaking
  let searching = $state(false);
  let searchTime = $state(0);
  let searchTimer: number | null = null;
  let unsubMatchFound: (() => void) | null = null;

  // Friends
  let friends = $state<Array<{ id: string; name: string; online: boolean; status: string }>>([]);
  let unsubFriends: (() => void) | null = null;
  let challenges = $state<Array<{ fromId: string; fromName: string; game: string }>>([]);
  let unsubChallenges: (() => void) | null = null;
  let addFriendInput = $state("");
  let showFriends = $state(false);
  let friendRequests = $state<FriendRequest[]>([]);
  let unsubFriendRequests: (() => void) | null = null;

  // Browser
  let rooms: Record<string, LobbyRoom> = $state({});
  let players: Record<string, LobbyPlayer> = $state({});

  // Cleanups
  let unsubRooms: (() => void) | null = null;
  let unsubRoom: (() => void) | null = null;
  let unsubPlayers: (() => void) | null = null;

  // Load player name from settings (Tauri backend or localStorage fallback)
  (async () => {
    try {
      const s: any = await invoke("get_settings");
      playerName = s.player_name || "Player";
    } catch {
      // Running outside Tauri — use localStorage
      playerName = localStorage.getItem("hw_player_name") || "Player";
    }
    if (playerName === "Player" || !playerName.trim()) {
      needsNameSetup = true;
      nameInput = "";
    } else {
      // Returning user — populate shortId and auto-connect
      getPlayerId();
      connect();
    }
  })();

  async function savePlayerName() {
    if (!nameInput.trim() || nameInput.trim().length < 2) return;
    playerName = nameInput.trim();
    needsNameSetup = false;
    // Always save to localStorage as fallback
    localStorage.setItem("hw_player_name", playerName);
    try {
      const s: any = await invoke("get_settings");
      await invoke("save_settings", {
        settings: { ...s, player_name: playerName }
      });
    } catch {
      // Outside Tauri — localStorage already saved above
    }
  }

  async function loadPlayerRating() {
    if (!playerId) return;
    try {
      // ensurePlayerRating creates if missing, and we always pass current name
      // so name changes propagate to the rating record on next connect
      playerRating = await ensurePlayerRating(playerId, playerName, "gnt4");
    } catch {}
  }

  // Generate unique player ID + derive permanent short ID
  function getPlayerId(): string {
    if (playerId) return playerId;
    let id = localStorage.getItem("hw_player_id");
    if (!id) {
      id = crypto.randomUUID();
      localStorage.setItem("hw_player_id", id);
    }
    playerId = id;
    // Derive permanent 8-char hex short ID from UUID (first 8 hex chars, no dashes)
    shortId = id.replace(/-/g, "").substring(0, 8).toUpperCase();
    return id;
  }

  async function connect() {
    error = "";
    const db = initFirebase();
    if (!db) {
      firebaseConfigured = false;
      error = "Firebase not configured. Set up a free Firebase project to enable online play.";
      return;
    }
    firebaseConfigured = true;
    connected = true;

    const id = getPlayerId();
    setPresence(id, playerName);

    // Subscribe to rooms, players, and friends
    unsubRooms = onRoomsChanged((r) => { rooms = r; });
    unsubPlayers = onPlayersChanged((p) => { players = p; });
    startFriendsListeners();
    loadPlayerRating();
  }

  // ── Matchmaking ──

  async function findMatch() {
    error = "";
    searching = true;
    searchTime = 0;
    playSfx("click");

    // Start search timer display
    searchTimer = window.setInterval(() => { searchTime++; }, 1000);

    try {
      const roomId = await joinMatchmakingQueue(playerId, playerName, "gnt4");
      if (roomId) {
        // Instantly matched!
        searching = false;
        clearSearchTimer();
        playSfx("match_found");
        await enterRoom(roomId, false);
      } else {
        // Waiting for match — listen for pairing
        unsubMatchFound = onMatchFound(playerId, "gnt4", async (roomId, opponentName) => {
          searching = false;
          clearSearchTimer();
          playSfx("match_found");
          await enterRoom(roomId, true);
        });
      }
    } catch (e: any) {
      error = `Matchmaking failed: ${e}`;
      searching = false;
      clearSearchTimer();
    }
  }

  function cancelSearch() {
    leaveMatchmakingQueue(playerId, "gnt4");
    if (unsubMatchFound) unsubMatchFound();
    searching = false;
    clearSearchTimer();
  }

  function clearSearchTimer() {
    if (searchTimer) { clearInterval(searchTimer); searchTimer = null; }
  }

  async function enterRoom(roomId: string, asHost: boolean) {
    currentRoomId = roomId;
    isHost = asHost;
    unsubRoom = onRoomChanged(roomId, (room) => {
      currentRoom = room;
      if (!room) { currentRoomId = ""; currentRoom = null; isHost = false; }
    });
  }

  function formatSearchTime(seconds: number): string {
    const m = Math.floor(seconds / 60);
    const s = seconds % 60;
    return m > 0 ? `${m}:${s.toString().padStart(2, "0")}` : `0:${s.toString().padStart(2, "0")}`;
  }

  // ── Friends ──

  function startFriendsListeners() {
    unsubFriends = onFriendsChanged(playerId, (f) => { friends = f; });
    unsubChallenges = onChallenges(playerId, (c) => {
      if (c.length > challenges.length) playSfx("match_found"); // New challenge notification
      challenges = c;
    });
    unsubFriendRequests = onFriendRequests(playerId, (r) => {
      if (r.length > friendRequests.length) playSfx("click"); // New request notification
      friendRequests = r;
    });
  }

  async function handleAddFriend() {
    if (!addFriendInput.trim()) return;
    const input = addFriendInput.trim().replace(/^#/, "").toUpperCase(); // Strip leading # if present
    // Search online players by short ID (first 8 hex chars of their UUID)
    const target = Object.entries(players).find(([id, _p]) => {
      const theirShortId = id.replace(/-/g, "").substring(0, 8).toUpperCase();
      return theirShortId === input;
    });
    if (target) {
      if (target[0] === playerId) {
        error = "You can't add yourself!";
        return;
      }
      await addFriend(playerId, target[0], target[1].name);
      addFriendInput = "";
      playSfx("click");
      error = `Friend request sent to ${target[1].name}!`;
      setTimeout(() => { if (error.includes("Friend request")) error = ""; }, 3000);
    } else {
      error = `Player #${input} not found online`;
    }
  }

  async function handleAcceptFriendRequest(req: FriendRequest) {
    await acceptFriendRequest(playerId, playerName, req.fromId, req.fromName);
    playSfx("click");
  }

  async function handleRejectFriendRequest(req: FriendRequest) {
    await rejectFriendRequest(playerId, req.fromId);
  }

  async function handleChallenge(friendId: string) {
    await sendChallenge(playerId, playerName, friendId, "gnt4");
    playSfx("click");
  }

  async function handleAcceptChallenge(challengerId: string) {
    try {
      const roomId = await acceptChallenge(playerId, playerName, challengerId);
      playSfx("match_found");
      await enterRoom(roomId, false);
    } catch (e: any) {
      error = `Failed to accept challenge: ${e}`;
    }
  }

  async function handleDeclineChallenge(challengerId: string) {
    await declineChallenge(playerId, challengerId);
  }

  function disconnect() {
    cancelSearch();
    if (currentRoomId) {
      leaveRoom(currentRoomId, playerId, isHost);
      currentRoomId = "";
      currentRoom = null;
    }
    removePresence(playerId);
    if (unsubRooms) unsubRooms();
    if (unsubRoom) unsubRoom();
    if (unsubPlayers) unsubPlayers();
    if (unsubFriends) unsubFriends();
    if (unsubChallenges) unsubChallenges();
    if (unsubFriendRequests) unsubFriendRequests();
    connected = false;
  }

  async function hostLobby() {
    error = "";
    try {
      const { roomId, code } = await createRoom(playerId, playerName);
      currentRoomId = roomId;
      isHost = true;

      // Watch this room for guest joining
      unsubRoom = onRoomChanged(roomId, (room) => {
        currentRoom = room;
        if (room && room.status === "full" && room.guest) {
          // Guest joined! Ready to start P2P connection
        }
        if (!room) {
          // Room was deleted
          currentRoomId = "";
          currentRoom = null;
          isHost = false;
        }
      });
    } catch (e: any) {
      error = "Failed to create room: " + e.toString();
    }
  }

  async function joinLobbyByCode() {
    if (!lobbyCode.trim()) return;
    error = "";
    try {
      const result = await joinRoom(lobbyCode.trim(), playerId, playerName);
      if (!result) {
        error = "Room not found or already full.";
        return;
      }
      currentRoomId = result.roomId;
      currentRoom = result.room;
      isHost = false;

      unsubRoom = onRoomChanged(result.roomId, (room) => {
        currentRoom = room;
        if (!room) {
          currentRoomId = "";
          currentRoom = null;
        }
      });
    } catch (e: any) {
      error = "Failed to join: " + e.toString();
    }
  }

  async function joinLobbyById(roomId: string) {
    const room = rooms[roomId];
    if (!room || room.status !== "waiting") return;
    lobbyCode = room.code;
    await joinLobbyByCode();
  }

  function leaveLobby() {
    if (currentRoomId) {
      leaveRoom(currentRoomId, playerId, isHost);
    }
    if (unsubRoom) unsubRoom();
    currentRoomId = "";
    currentRoom = null;
    isHost = false;
  }

  function copyCode(code: string) {
    navigator.clipboard.writeText(code);
    playSfx("click");
  }

  // ── P2P Connection + Game Launch ──

  /** Launch Dolphin with GNT4 if it's not already running, then wait for it to be ready. */
  async function ensureDolphinRunning(): Promise<boolean> {
    // Try attaching first — if it works, Dolphin is already running with a game
    try {
      await invoke("dolphin_mem_attach");
      return true;
    } catch {
      // Not running or no game loaded — launch it
    }

    netplayStatus = "Launching Dolphin...";
    try {
      await invoke("launch_dolphin", { mode: "netplay", isoOverride: null });
    } catch (e: any) {
      netplayStatus = `Failed to launch Dolphin: ${e}`;
      return false;
    }

    // Wait for Dolphin to load the game (poll for memory attachment)
    netplayStatus = "Waiting for game to load...";
    for (let attempt = 0; attempt < 30; attempt++) {
      await new Promise((r) => setTimeout(r, 2000));
      try {
        await invoke("dolphin_mem_attach");
        netplayStatus = "Dolphin ready!";
        return true;
      } catch {
        // Not ready yet
      }
    }
    netplayStatus = "Timed out waiting for Dolphin. Launch it manually and load GNT4.";
    return false;
  }

  async function startMatch() {
    if (!currentRoom) return;
    error = "";
    connectionState = "connecting";
    netplayStatus = "Starting netplay session...";

    try {
      // Get settings for input delay / rollback
      const settings: any = await invoke("get_settings");
      const playerIdx = isHost ? 0 : 1; // Host = P1, Guest = P2

      // Auto-launch Dolphin if not running
      const dolphinReady = await ensureDolphinRunning();

      // Start UDP session
      const port: number = await invoke("netplay_start", {
        playerId: playerIdx,
        inputDelay: settings.input_delay ?? 2,
        maxRollback: settings.max_rollback ?? 7,
        port: 0, // Let OS assign port
      });
      localPort = port;
      netplayStatus = `UDP bound on port ${port}. Discovering public address...`;

      // Discover our public IP:port via STUN
      let publicAddr = "";
      try {
        publicAddr = await invoke("stun_discover") as string;
        netplayStatus = `Public address: ${publicAddr}. Exchanging with peer...`;
      } catch {
        // STUN failed — fall back to local port only (LAN play)
        publicAddr = `127.0.0.1:${port}`;
        netplayStatus = `STUN failed, using local address. Exchanging...`;
      }

      // Exchange addresses via Firebase signaling
      await sendSignal(currentRoomId, playerId, {
        type: "udp_ready",
        port: port,
        publicAddr: publicAddr,
      });

      // Listen for peer's address
      // onSignals callback receives (signal, fromId) — single signal, not array
      const unsubSignal = onSignals(currentRoomId, playerId, async (signal: any) => {
        if (signal.type === "udp_ready" && (signal.publicAddr || signal.port)) {
          // Use peer's public address from STUN, fall back to localhost
          const peerAddr = signal.publicAddr || `127.0.0.1:${signal.port}`;

          netplayStatus = `Hole punching to ${peerAddr}...`;

          // Attempt UDP hole punch
          try {
            const punched = await invoke("stun_hole_punch", { peerAddress: peerAddr });
            if (punched) {
              netplayStatus = `Hole punch succeeded! Connecting...`;
            } else {
              netplayStatus = `Hole punch timed out, trying direct connect...`;
            }
          } catch {
            netplayStatus = `Connecting directly to ${peerAddr}...`;
          }

          await invoke("netplay_connect", { peerAddress: peerAddr });

          connectionState = "connected";
          netplayStatus = "Connected! Starting rollback...";
          playSfx("match_found");

          // Ensure Dolphin is attached (may have launched during connection)
          if (!dolphinReady) {
            const nowReady = await ensureDolphinRunning();
            if (!nowReady) {
              netplayStatus = "Connected but Dolphin not ready. Launch Dolphin with GNT4 and try again.";
              unsubSignal();
              return;
            }
          }

          netplayStatus = "Attached to Dolphin. Starting rollback engine...";

          // Start rollback engine
          try {
            await invoke("rollback_start", {
              inputDelay: settings.input_delay ?? 2,
              maxRollback: settings.max_rollback ?? 7,
              localPlayer: playerIdx,
              ranked: ranked,
            });

            connectionState = "playing";
            netplayStatus = "Rollback active!";
            playSfx("match_start");
            startStatsPolling();
          } catch (e: any) {
            netplayStatus = `Rollback start failed: ${e}`;
          }

          unsubSignal();
        }
      });
    } catch (e: any) {
      error = `Match start failed: ${e}`;
      connectionState = "idle";
      netplayStatus = "";
    }
  }

  // Player index for direct connect — host is P1 (0), joiner is P2 (1)
  let directAsPlayer2 = $state(false);

  async function directConnect() {
    if (!directIp.trim()) return;
    error = "";
    connectionState = "connecting";
    const playerIdx = directAsPlayer2 ? 1 : 0;

    try {
      const settings: any = await invoke("get_settings");

      // Auto-launch Dolphin if needed
      netplayStatus = "Checking Dolphin...";
      const dolphinReady = await ensureDolphinRunning();

      // Start UDP session
      const port: number = await invoke("netplay_start", {
        playerId: playerIdx,
        inputDelay: settings.input_delay ?? 2,
        maxRollback: settings.max_rollback ?? 7,
        port: 0,
      });
      localPort = port;
      netplayStatus = `Bound on port ${port}. Connecting to ${directIp}...`;

      await invoke("netplay_connect", { peerAddress: directIp.trim() });
      connectionState = "connected";
      netplayStatus = `Connected to ${directIp}!`;
      playSfx("match_found");

      // Ensure Dolphin is attached
      if (!dolphinReady) {
        await ensureDolphinRunning();
      }

      try {
        await invoke("rollback_start", {
          inputDelay: settings.input_delay ?? 2,
          maxRollback: settings.max_rollback ?? 7,
          localPlayer: playerIdx,
          ranked: ranked,
        });
        connectionState = "playing";
        netplayStatus = "Rollback active!";
        playSfx("match_start");
        startStatsPolling();
      } catch (e: any) {
        netplayStatus = `Connected but rollback failed: ${e}. Is Dolphin running with GNT4?`;
      }
    } catch (e: any) {
      error = `Direct connect failed: ${e}`;
      connectionState = "idle";
      netplayStatus = "";
    }
  }

  function startStatsPolling() {
    if (statsTimer) clearInterval(statsTimer);
    statsTimer = setInterval(async () => {
      try {
        const stats: any = await invoke("rollback_stats");
        rollbackStats = stats;
      } catch {}
    }, 500) as unknown as number; // Poll every 500ms
  }

  function stopStatsPolling() {
    if (statsTimer) { clearInterval(statsTimer); statsTimer = null; }
    rollbackStats = null;
  }

  async function stopNetplay() {
    stopStatsPolling();
    try {
      await invoke("rollback_stop");
      await invoke("netplay_stop");
      await invoke("dolphin_mem_detach");
    } catch {}
    connectionState = "idle";
    netplayStatus = "";
  }

  // Online player count
  let onlineCount = $derived(Object.keys(players).length);
  let waitingRooms = $derived(
    Object.entries(rooms).filter(([_, r]) => r.status === "waiting")
  );

  $effect(() => {
    return () => {
      disconnect();
    };
  });
</script>

<div class="play-online">
  <h2 class="page-title">PLAY ONLINE</h2>
  <p class="page-desc">Connect with an opponent using rollback netcode</p>

  {#if needsNameSetup}
    <!-- First-time signup -->
    <div class="name-setup">
      <div class="panel name-panel">
        {#if !shortId}
          <h3 class="panel-title">WELCOME TO HOWLINGWIND</h3>
          <p class="panel-desc">Choose a display name for online play. You can change it later.</p>
          <div class="input-group">
            <input
              type="text"
              placeholder="Enter your name..."
              bind:value={nameInput}
              maxlength="20"
              class="code-input"
              onkeydown={(e: KeyboardEvent) => { if (e.key === "Enter" && nameInput.trim().length >= 2) { savePlayerName(); getPlayerId(); } }}
              style="letter-spacing: 0; text-transform: none; font-family: 'Rajdhani', sans-serif;"
            />
            <button class="btn btn-primary" onclick={() => { savePlayerName(); getPlayerId(); }} disabled={!nameInput.trim() || nameInput.trim().length < 2}>
              Create Profile
            </button>
          </div>
          <p class="name-hint">Minimum 2 characters. This is how opponents will see you.</p>
        {:else}
          <h3 class="panel-title">YOU'RE ALL SET</h3>
          <div class="signup-result">
            <p class="signup-name">{playerName}</p>
            <p class="signup-id-label">Your permanent player ID:</p>
            <p class="signup-id">#{shortId}</p>
            <p class="panel-desc">Share this ID with friends so they can add you. Your name can change, but this ID is forever.</p>
            <button class="btn btn-primary" onclick={() => { needsNameSetup = false; connect(); }}>
              Enter the Storm
            </button>
          </div>
        {/if}
      </div>
    </div>
  {:else if !connected}
    <!-- Connecting to lobby -->
    <div class="connect-section">
      <div class="panel" style="text-align: center; padding: 32px;">
        {#if error}
          <p class="error">{error}</p>
          {#if !firebaseConfigured}
            <div class="setup-hint">
              <p>To enable online play:</p>
              <ol>
                <li>Create a free Firebase project at <strong>console.firebase.google.com</strong></li>
                <li>Enable Realtime Database (free tier)</li>
                <li>Copy your config into <code>src/lib/firebase.ts</code></li>
              </ol>
            </div>
          {/if}
          <button class="btn btn-primary" onclick={connect} style="margin-top: 12px;">Retry</button>
        {:else}
          <div class="spinner"></div>
          <p class="panel-desc" style="margin-top: 12px;">Connecting to lobby...</p>
        {/if}
      </div>
    </div>
  {:else if currentRoomId && currentRoom}
    <!-- In a room -->
    <div class="room-view">
      <div class="panel room-panel">
        <div class="room-header">
          <h3 class="panel-title">{isHost ? "YOUR LOBBY" : "JOINED LOBBY"}</h3>
          <button class="btn btn-danger" onclick={leaveLobby}>Leave</button>
        </div>

        <div class="code-display">
          <span class="code-label">Room Code</span>
          <div class="code-value">
            <span>{currentRoom.code}</span>
            <button class="btn-copy" onclick={() => copyCode(currentRoom!.code)} title="Copy">COPY</button>
          </div>
        </div>

        <div class="room-players">
          <div class="room-player host">
            <span class="player-badge">HOST</span>
            <span class="player-name">{currentRoom.hostName}</span>
            <span class="player-ready">Ready</span>
          </div>
          {#if currentRoom.guest}
            <div class="room-player guest">
              <span class="player-badge guest-badge">GUEST</span>
              <span class="player-name">{currentRoom.guestName}</span>
              <span class="player-ready">Ready</span>
            </div>
          {:else}
            <div class="room-player empty">
              <div class="spinner"></div>
              <span>Waiting for opponent...</span>
            </div>
          {/if}
        </div>

        {#if currentRoom.status === "full"}
          {#if connectionState === "idle"}
            <button class="btn btn-primary btn-start" onclick={startMatch}>
              START MATCH
            </button>
            <p class="match-hint">P2P connection will be established automatically</p>
          {:else}
            <div class="connection-status">
              <div class="spinner"></div>
              <span class="status-text">{netplayStatus}</span>
            </div>
            {#if connectionState === "playing"}
              <button class="btn btn-danger" onclick={stopNetplay}>STOP NETPLAY</button>
            {/if}
          {/if}
        {/if}
      </div>
    </div>
  {:else}
    <!-- Connected, lobby browser -->
    <div class="lobby-main">
      <!-- Status bar with profile -->
      <div class="status-bar">
        <div class="status-online">
          <span class="status-dot"></span>
          <span>{onlineCount} player{onlineCount !== 1 ? "s" : ""} online</span>
        </div>
        <div class="player-profile">
          <span class="profile-name">{playerName}</span>
          {#if shortId}
            <span class="profile-id">#{shortId}</span>
          {/if}
          {#if playerRating}
            <span class="profile-elo">{playerRating.elo} ELO</span>
            <span class="profile-record">{playerRating.wins}W - {playerRating.losses}L</span>
            {#if playerRating.streak > 2}
              <span class="profile-streak win-streak">{playerRating.streak} streak</span>
            {:else if playerRating.streak < -2}
              <span class="profile-streak loss-streak">{Math.abs(playerRating.streak)} streak</span>
            {/if}
          {/if}
        </div>
        <button class="btn-disconnect" onclick={disconnect}>Disconnect</button>
      </div>

      <!-- Mode Toggle -->
      <div class="mode-toggle">
        <button class="mode-btn" class:active={ranked} class:disabled={searching || connectionState !== "idle"} onclick={() => { if (!searching && connectionState === "idle") ranked = true; }} disabled={searching || connectionState !== "idle"}>
          <span class="mode-icon">&#9733;</span>
          <span class="mode-label">RANKED</span>
          <span class="mode-desc">ELO tracked, Best of 3 sets</span>
        </button>
        <button class="mode-btn" class:active={!ranked} class:disabled={searching || connectionState !== "idle"} onclick={() => { if (!searching && connectionState === "idle") ranked = false; }} disabled={searching || connectionState !== "idle"}>
          <span class="mode-icon">&#9889;</span>
          <span class="mode-label">UNRANKED</span>
          <span class="mode-desc">Casual play, no ELO changes</span>
        </button>
      </div>

      <!-- Matchmaking - big prominent button -->
      <div class="matchmaking-panel">
        {#if !searching}
          <button class="btn-find-match" onclick={findMatch}>
            <span class="find-icon">&#9889;</span>
            <span class="find-text">FIND MATCH</span>
            <span class="find-desc">Auto-pair with another player</span>
          </button>
        {:else}
          <div class="searching-panel">
            <div class="search-spinner"></div>
            <div class="search-info">
              <span class="search-text">SEARCHING FOR OPPONENT...</span>
              <span class="search-time">{formatSearchTime(searchTime)}</span>
            </div>
            <button class="btn btn-danger" onclick={cancelSearch}>Cancel</button>
          </div>
        {/if}
      </div>

      <!-- Challenges -->
      {#if challenges.length > 0}
        <div class="challenges-panel">
          <h3 class="section-title">INCOMING CHALLENGES</h3>
          {#each challenges as challenge}
            <div class="challenge-item">
              <span class="challenge-name">{challenge.fromName}</span>
              <span class="challenge-game">wants to fight!</span>
              <button class="btn btn-primary btn-sm" onclick={() => handleAcceptChallenge(challenge.fromId)}>Accept</button>
              <button class="btn btn-danger btn-sm" onclick={() => handleDeclineChallenge(challenge.fromId)}>Decline</button>
            </div>
          {/each}
        </div>
      {/if}

      <div class="panels">
        <!-- Host / Join -->
        <div class="panel">
          <h3 class="panel-title">HOST LOBBY</h3>
          <p class="panel-desc">Create a room and share the code</p>
          <button class="btn btn-primary" onclick={hostLobby}>Create Lobby</button>
        </div>

        <div class="divider-vertical"><span>OR</span></div>

        <div class="panel">
          <h3 class="panel-title">JOIN LOBBY</h3>
          <p class="panel-desc">Enter a room code to connect</p>
          <div class="input-group">
            <input
              type="text"
              placeholder="Enter code..."
              bind:value={lobbyCode}
              maxlength="6"
              class="code-input"
            />
            <button class="btn btn-primary" onclick={joinLobbyByCode} disabled={!lobbyCode.trim()}>
              Join
            </button>
          </div>
        </div>
      </div>

      <!-- Friends Panel -->
      <div class="friends-section">
        <div class="friends-header" >
          <h3 class="section-title" onclick={() => showFriends = !showFriends} style="cursor: pointer;">
            FRIENDS ({friends.filter(f => f.online).length}/{friends.length} online)
            <span class="toggle-arrow">{showFriends ? "▲" : "▼"}</span>
          </h3>
        </div>

        {#if showFriends}
          <div class="friends-content">
            <div class="add-friend-row">
              <input
                type="text"
                placeholder="Add by ID (e.g. A1B2C3D4)..."
                bind:value={addFriendInput}
                class="friend-input"
              />
              <button class="btn btn-secondary btn-sm" onclick={handleAddFriend}>Add</button>
            </div>

            {#if friendRequests.length > 0}
              <div class="friend-requests">
                <p class="requests-label">FRIEND REQUESTS</p>
                {#each friendRequests as req}
                  <div class="friend-request-item">
                    <span class="req-name">{req.fromName}</span>
                    <span class="req-text">wants to be friends</span>
                    <button class="btn btn-primary btn-sm" onclick={() => handleAcceptFriendRequest(req)}>Accept</button>
                    <button class="btn btn-danger btn-sm" onclick={() => handleRejectFriendRequest(req)}>Reject</button>
                  </div>
                {/each}
              </div>
            {/if}

            {#if friends.length > 0}
              <div class="friends-list">
                {#each friends as friend}
                  <div class="friend-item" class:online={friend.online}>
                    <span class="friend-dot" class:active={friend.online}></span>
                    <span class="friend-name">{friend.name}</span>
                    <span class="friend-status">{friend.online ? friend.status : "offline"}</span>
                    {#if friend.online && friend.status === "idle"}
                      <button class="btn-challenge" onclick={() => handleChallenge(friend.id)}>Challenge</button>
                    {/if}
                    <button class="btn-remove-friend" onclick={() => removeFriend(playerId, friend.id)} title="Remove">&#10005;</button>
                  </div>
                {/each}
              </div>
            {:else}
              <p class="no-friends">No friends yet. Add players by their display name.</p>
            {/if}
          </div>
        {/if}
      </div>

      {#if error}
        <p class="error">{error}</p>
      {/if}

      <!-- Lobby browser -->
      {#if waitingRooms.length > 0}
        <div class="lobby-browser">
          <h3 class="section-title">OPEN LOBBIES</h3>
          <div class="lobby-list">
            {#each waitingRooms as [roomId, room]}
              <div class="lobby-item">
                <span class="lobby-host">{room.hostName}</span>
                <span class="lobby-code">{room.code}</span>
                <span class="lobby-status">Waiting</span>
                <button class="btn btn-secondary btn-join" onclick={() => joinLobbyById(roomId)}>
                  Join
                </button>
              </div>
            {/each}
          </div>
        </div>
      {:else}
        <div class="no-lobbies">
          <p>No open lobbies. Create one or wait for others to appear.</p>
        </div>
      {/if}

      <!-- Direct connect — prominent for LAN testing -->
      <div class="direct-connect panel" style="margin-top: 24px; background: var(--bg-card); border: 1px solid var(--border); border-radius: 12px; padding: 24px;">
        <h3 class="panel-title">DIRECT CONNECT</h3>
        <p class="panel-desc">Connect directly via IP address — perfect for LAN or testing</p>

        <!-- Player role toggle -->
        <div class="player-toggle" style="display: flex; gap: 8px; margin-top: 8px;">
          <button
            class="mode-btn-sm"
            class:active={!directAsPlayer2}
            onclick={() => directAsPlayer2 = false}
            disabled={connectionState !== "idle"}
          >P1 (Host)</button>
          <button
            class="mode-btn-sm"
            class:active={directAsPlayer2}
            onclick={() => directAsPlayer2 = true}
            disabled={connectionState !== "idle"}
          >P2 (Joiner)</button>
        </div>

        <div class="input-group" style="margin-top: 12px;">
          <input
            type="text"
            placeholder="IP:Port (e.g. 192.168.1.100:7654)"
            class="ip-input"
            bind:value={directIp}
            onkeydown={(e: KeyboardEvent) => { if (e.key === "Enter" && directIp.trim()) directConnect(); }}
          />
          <button
            class="btn btn-primary"
            onclick={directConnect}
            disabled={!directIp.trim() || connectionState !== "idle"}
          >
            {connectionState !== "idle" ? "Connecting..." : "Connect"}
          </button>
        </div>
        {#if localPort > 0}
          <p class="port-info">Your port: <strong>{localPort}</strong> — the other player connects to <strong>YOUR_IP:{localPort}</strong></p>
        {/if}
        {#if netplayStatus && connectionState !== "idle"}
          <div class="connection-status" style="margin-top: 12px;">
            {#if connectionState !== "playing"}<div class="spinner"></div>{/if}
            <span class="status-text">{netplayStatus}</span>
          </div>
          {#if connectionState === "playing"}
            <button class="btn btn-danger" onclick={stopNetplay} style="margin-top: 8px;">STOP NETPLAY</button>
          {/if}
        {/if}
      </div>
    </div>
  {/if}
</div>

<!-- Connection Quality Overlay (shown during gameplay) -->
{#if connectionState === "playing" && rollbackStats && showOverlay}
  <div class="quality-overlay">
    <div class="overlay-header">
      <span class="overlay-title">NETPLAY</span>
      <button class="overlay-close" onclick={() => showOverlay = false}>x</button>
    </div>
    <div class="overlay-stats">
      <div class="stat-row">
        <span class="stat-label">PING</span>
        <span class="stat-value" class:stat-good={rollbackStats.ping_ms < 50} class:stat-warn={rollbackStats.ping_ms >= 50 && rollbackStats.ping_ms < 100} class:stat-bad={rollbackStats.ping_ms >= 100}>
          {rollbackStats.ping_ms.toFixed(0)}ms
        </span>
      </div>
      <div class="stat-row">
        <span class="stat-label">ROLLBACKS</span>
        <span class="stat-value">{rollbackStats.rollback_count}</span>
      </div>
      <div class="stat-row">
        <span class="stat-label">AHEAD</span>
        <span class="stat-value" class:stat-warn={rollbackStats.frames_ahead > 3} class:stat-bad={rollbackStats.frames_ahead > 5}>
          {rollbackStats.frames_ahead}f
        </span>
      </div>
      <div class="stat-row">
        <span class="stat-label">PREDICT</span>
        <span class="stat-value">{rollbackStats.prediction_success_rate.toFixed(0)}%</span>
      </div>
      {#if rollbackStats.desync_detected}
        <div class="stat-row desync-warning">
          <span class="stat-label">DESYNC</span>
          <span class="stat-value stat-bad">DETECTED</span>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .play-online { padding: 32px 40px; }
  .page-title { font-family: 'Orbitron', monospace; font-size: 24px; font-weight: 700; letter-spacing: 3px; color: var(--text-primary); }
  .page-desc { color: var(--text-secondary); margin-top: 4px; font-size: 14px; }

  .connect-section { margin-top: 32px; }
  .connect-row { display: flex; gap: 12px; align-items: center; }
  .error { color: #ef4444; font-size: 13px; margin-top: 8px; }

  .setup-hint { margin-top: 12px; padding: 16px; background: var(--bg-primary); border: 1px solid var(--border); border-radius: 8px; font-size: 12px; color: var(--text-secondary); line-height: 1.6; }
  .setup-hint ol { margin-top: 8px; padding-left: 20px; }
  .setup-hint code { background: var(--bg-card-hover); padding: 2px 6px; border-radius: 3px; font-size: 11px; color: var(--wind-cyan); }

  .lobby-main { margin-top: 24px; }

  .status-bar { display: flex; align-items: center; gap: 16px; padding: 12px 16px; background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; margin-bottom: 24px; }
  .status-online { display: flex; align-items: center; gap: 8px; font-size: 13px; font-weight: 600; color: var(--text-primary); }
  .status-dot { width: 8px; height: 8px; border-radius: 50%; background: #22c55e; box-shadow: 0 0 8px rgba(34, 197, 94, 0.5); }
  .status-name { font-size: 12px; color: var(--text-muted); margin-left: auto; }
  .btn-disconnect { padding: 6px 14px; background: none; border: 1px solid var(--border); border-radius: 6px; color: var(--text-muted); font-size: 11px; font-weight: 600; cursor: pointer; transition: all 0.15s; }
  .btn-disconnect:hover { color: #ef4444; border-color: #ef4444; }

  .panels { display: flex; gap: 24px; align-items: stretch; }
  .panel { flex: 1; background: var(--bg-card); border: 1px solid var(--border); border-radius: 12px; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
  .panel-title { font-family: 'Orbitron', monospace; font-size: 13px; font-weight: 700; letter-spacing: 2px; color: var(--wind-cyan); }
  .panel-desc { font-size: 13px; color: var(--text-secondary); }

  .divider-vertical { display: flex; align-items: center; color: var(--text-muted); font-size: 12px; font-weight: 700; letter-spacing: 2px; }

  .btn { padding: 10px 24px; border-radius: 8px; font-size: 13px; font-weight: 700; letter-spacing: 1px; transition: all 0.2s ease; border: none; cursor: pointer; }
  .btn-primary { background: linear-gradient(135deg, var(--accent-primary), var(--wind-cyan)); color: white; }
  .btn-primary:hover { box-shadow: 0 0 20px rgba(34, 211, 238, 0.3); transform: translateY(-1px); }
  .btn-primary:disabled { opacity: 0.4; cursor: not-allowed; transform: none; box-shadow: none; }
  .btn-secondary { background: var(--bg-card-hover); color: var(--text-primary); border: 1px solid var(--border); }
  .btn-secondary:hover { border-color: var(--accent-primary); }
  .btn-danger { padding: 6px 16px; background: none; border: 1px solid rgba(239, 68, 68, 0.4); color: #ef4444; font-size: 12px; font-weight: 600; border-radius: 6px; cursor: pointer; transition: all 0.15s; }
  .btn-danger:hover { background: rgba(239, 68, 68, 0.1); }

  .code-display { display: flex; flex-direction: column; gap: 6px; }
  .code-label { font-size: 11px; color: var(--text-muted); text-transform: uppercase; letter-spacing: 1px; }
  .code-value { display: flex; align-items: center; gap: 12px; font-family: 'Orbitron', monospace; font-size: 28px; font-weight: 900; letter-spacing: 6px; color: var(--wind-cyan); text-shadow: 0 0 10px rgba(34, 211, 238, 0.3); }
  .btn-copy { font-size: 11px; background: var(--bg-primary); color: var(--text-muted); padding: 4px 12px; border-radius: 4px; border: 1px solid var(--border); font-weight: 600; cursor: pointer; transition: all 0.15s; }
  .btn-copy:hover { color: var(--text-primary); border-color: var(--text-muted); }

  .input-group { display: flex; gap: 8px; }
  .code-input, .ip-input { flex: 1; padding: 10px 16px; background: var(--bg-primary); border: 1px solid var(--border); border-radius: 8px; color: var(--text-primary); font-size: 14px; font-family: 'Orbitron', monospace; letter-spacing: 3px; text-transform: uppercase; }
  .ip-input { letter-spacing: 0; text-transform: none; font-family: 'Rajdhani', sans-serif; }
  .code-input:focus, .ip-input:focus { border-color: var(--wind-cyan); box-shadow: 0 0 10px rgba(34, 211, 238, 0.1); outline: none; }
  .code-input::placeholder, .ip-input::placeholder { color: var(--text-muted); letter-spacing: 0; text-transform: none; font-family: 'Rajdhani', sans-serif; }

  /* Room view */
  .room-view { margin-top: 32px; }
  .room-panel { max-width: 500px; }
  .room-header { display: flex; justify-content: space-between; align-items: center; }
  .room-players { display: flex; flex-direction: column; gap: 8px; }
  .room-player { display: flex; align-items: center; gap: 10px; padding: 12px 14px; background: var(--bg-primary); border: 1px solid var(--border); border-radius: 8px; }
  .room-player.empty { color: var(--text-muted); font-size: 13px; }
  .player-badge { font-family: 'Orbitron', monospace; font-size: 9px; font-weight: 700; letter-spacing: 1px; padding: 3px 8px; border-radius: 4px; background: var(--accent-primary); color: white; }
  .guest-badge { background: var(--wind-cyan); }
  .player-name { font-size: 14px; font-weight: 600; color: var(--text-primary); }
  .player-ready { margin-left: auto; font-size: 11px; color: #22c55e; font-weight: 600; }
  .btn-start { width: 100%; padding: 14px; font-size: 14px; }
  .match-hint { font-size: 11px; color: var(--text-muted); text-align: center; }

  /* Lobby browser */
  .lobby-browser { margin-top: 32px; }
  .section-title { font-family: 'Orbitron', monospace; font-size: 12px; font-weight: 700; letter-spacing: 2px; color: var(--text-secondary); margin-bottom: 12px; }
  .lobby-list { display: flex; flex-direction: column; gap: 6px; }
  .lobby-item { display: flex; align-items: center; gap: 16px; padding: 12px 16px; background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; transition: border-color 0.15s; }
  .lobby-item:hover { border-color: var(--text-muted); }
  .lobby-host { font-size: 14px; font-weight: 600; color: var(--text-primary); flex: 1; }
  .lobby-code { font-family: 'Orbitron', monospace; font-size: 12px; font-weight: 700; color: var(--wind-cyan); letter-spacing: 2px; }
  .lobby-status { font-size: 11px; color: #22c55e; font-weight: 600; }
  .btn-join { padding: 6px 16px; font-size: 11px; }

  .no-lobbies { margin-top: 32px; padding: 24px; text-align: center; color: var(--text-muted); font-size: 13px; background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; }

  .direct-connect { margin-top: 24px; }

  /* Player toggle (P1/P2 for direct connect) */
  .mode-btn-sm { padding: 8px 16px; background: var(--bg-primary); border: 2px solid var(--border); border-radius: 6px; font-family: 'Orbitron', monospace; font-size: 11px; font-weight: 700; letter-spacing: 1px; color: var(--text-muted); cursor: pointer; transition: all 0.2s ease; }
  .mode-btn-sm:hover { border-color: var(--text-muted); color: var(--text-primary); }
  .mode-btn-sm.active { border-color: var(--wind-cyan); color: var(--wind-cyan); background: rgba(34, 211, 238, 0.05); }
  .mode-btn-sm:disabled { opacity: 0.4; cursor: not-allowed; }

  .spinner { width: 16px; height: 16px; border: 2px solid var(--border); border-top-color: var(--wind-cyan); border-radius: 50%; animation: spin 0.8s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }

  .connection-status { display: flex; align-items: center; gap: 10px; padding: 12px; background: var(--bg-primary); border: 1px solid var(--border); border-radius: 8px; margin-top: 8px; }
  .connection-status .status-text { font-size: 12px; color: var(--text-secondary); }
  .port-info { font-size: 11px; color: var(--text-muted); margin-top: 8px; }
  .port-info strong { color: var(--wind-cyan); font-family: 'Orbitron', monospace; }

  /* Mode Toggle */
  .mode-toggle { display: flex; gap: 12px; margin-bottom: 20px; }
  .mode-btn { flex: 1; display: flex; flex-direction: column; align-items: center; gap: 4px; padding: 16px; background: var(--bg-card); border: 2px solid var(--border); border-radius: 10px; cursor: pointer; transition: all 0.2s ease; }
  .mode-btn:hover { border-color: var(--text-muted); }
  .mode-btn.active { border-color: var(--wind-cyan); background: rgba(34, 211, 238, 0.05); }
  .mode-icon { font-size: 20px; }
  .mode-label { font-family: 'Orbitron', monospace; font-size: 14px; font-weight: 900; letter-spacing: 2px; color: var(--text-primary); }
  .mode-btn.active .mode-label { color: var(--wind-cyan); }
  .mode-desc { font-size: 11px; color: var(--text-muted); }
  .mode-btn.disabled { opacity: 0.4; cursor: not-allowed; }
  .mode-btn.disabled:hover { border-color: var(--border); }

  /* Matchmaking */
  .matchmaking-panel { margin-bottom: 24px; }
  .btn-find-match { width: 100%; padding: 20px; background: linear-gradient(135deg, var(--accent-primary), var(--wind-cyan)); border-radius: 12px; display: flex; align-items: center; gap: 16px; transition: all 0.3s ease; position: relative; overflow: hidden; }
  .btn-find-match:hover { box-shadow: 0 0 30px rgba(34, 211, 238, 0.4); transform: translateY(-2px); }
  .find-icon { font-size: 28px; }
  .find-text { font-family: 'Orbitron', monospace; font-size: 18px; font-weight: 900; letter-spacing: 3px; color: white; }
  .find-desc { font-size: 12px; color: rgba(255,255,255,0.7); margin-left: auto; }
  .searching-panel { display: flex; align-items: center; gap: 16px; padding: 20px; background: var(--bg-card); border: 2px solid var(--wind-cyan); border-radius: 12px; animation: searchPulse 2s ease infinite; }
  @keyframes searchPulse { 0%, 100% { border-color: rgba(34, 211, 238, 0.3); } 50% { border-color: rgba(34, 211, 238, 0.8); } }
  .search-spinner { width: 24px; height: 24px; border: 3px solid var(--border); border-top-color: var(--wind-cyan); border-radius: 50%; animation: spin 0.8s linear infinite; }
  .search-info { flex: 1; }
  .search-text { font-family: 'Orbitron', monospace; font-size: 13px; font-weight: 700; letter-spacing: 2px; color: var(--wind-cyan); display: block; }
  .search-time { font-family: 'Orbitron', monospace; font-size: 20px; font-weight: 700; color: var(--text-primary); }

  /* Challenges */
  .challenges-panel { margin-bottom: 16px; padding: 16px; background: rgba(34, 211, 238, 0.05); border: 1px solid rgba(34, 211, 238, 0.2); border-radius: 12px; }
  .challenge-item { display: flex; align-items: center; gap: 10px; padding: 8px 0; }
  .challenge-name { font-weight: 700; color: var(--wind-cyan); font-size: 14px; }
  .challenge-game { font-size: 12px; color: var(--text-muted); flex: 1; }
  .btn-sm { padding: 6px 14px !important; font-size: 11px !important; }

  /* Friends */
  .friends-section { margin-top: 24px; background: var(--bg-card); border: 1px solid var(--border); border-radius: 12px; padding: 16px; }
  .friends-header { display: flex; justify-content: space-between; align-items: center; }
  .toggle-arrow { font-size: 10px; margin-left: 8px; }
  .friends-content { margin-top: 12px; }
  .add-friend-row { display: flex; gap: 8px; margin-bottom: 12px; }
  .friend-input { flex: 1; padding: 8px 12px; background: var(--bg-primary); border: 1px solid var(--border); border-radius: 6px; color: var(--text-primary); font-size: 13px; }
  .friend-input:focus { border-color: var(--wind-cyan); outline: none; }
  .friends-list { display: flex; flex-direction: column; gap: 4px; }
  .friend-item { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border-radius: 6px; transition: background 0.15s; }
  .friend-item:hover { background: var(--bg-primary); }
  .friend-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--text-muted); }
  .friend-dot.active { background: #22c55e; box-shadow: 0 0 6px rgba(34, 197, 94, 0.5); }
  .friend-name { font-size: 13px; font-weight: 600; color: var(--text-primary); flex: 1; }
  .friend-status { font-size: 11px; color: var(--text-muted); }
  .btn-challenge { padding: 4px 12px; background: var(--accent-primary); color: white; font-size: 10px; font-weight: 700; border-radius: 4px; font-family: 'Orbitron', monospace; letter-spacing: 1px; transition: all 0.15s; }
  .btn-challenge:hover { box-shadow: 0 0 10px rgba(34, 211, 238, 0.3); }
  .btn-remove-friend { background: none; border: none; color: var(--text-muted); font-size: 12px; padding: 4px; cursor: pointer; opacity: 0; transition: opacity 0.15s; }
  .friend-item:hover .btn-remove-friend { opacity: 1; }
  .btn-remove-friend:hover { color: #ef4444; }
  .no-friends { font-size: 12px; color: var(--text-muted); text-align: center; padding: 12px; }

  /* Friend requests */
  .friend-requests { margin-bottom: 12px; padding: 10px; background: rgba(34, 211, 238, 0.05); border: 1px solid rgba(34, 211, 238, 0.15); border-radius: 8px; }
  .requests-label { font-family: 'Orbitron', monospace; font-size: 10px; font-weight: 700; letter-spacing: 1px; color: var(--wind-cyan); margin-bottom: 8px; }
  .friend-request-item { display: flex; align-items: center; gap: 8px; padding: 6px 0; }
  .req-name { font-weight: 700; color: var(--text-primary); font-size: 13px; }
  .req-text { font-size: 11px; color: var(--text-muted); flex: 1; }

  /* Name setup */
  .name-setup { margin-top: 32px; }
  .signup-result { text-align: center; padding: 16px 0; }
  .signup-name { font-size: 24px; font-weight: 700; color: var(--text-primary); margin-bottom: 8px; }
  .signup-id-label { font-size: 12px; color: var(--text-muted); margin-bottom: 4px; text-transform: uppercase; letter-spacing: 1px; }
  .signup-id { font-family: 'Orbitron', monospace; font-size: 28px; font-weight: 700; color: var(--wind-cyan); letter-spacing: 3px; margin-bottom: 16px; user-select: all; }
  .name-panel { max-width: 480px; }
  .name-hint { font-size: 11px; color: var(--text-muted); margin-top: 4px; }

  /* Player profile in status bar */
  .player-profile { display: flex; align-items: center; gap: 10px; margin-left: auto; }
  .profile-name { font-size: 14px; font-weight: 700; color: var(--text-primary); }
  .profile-id { font-family: 'Orbitron', monospace; font-size: 10px; color: var(--text-muted); letter-spacing: 1px; cursor: pointer; user-select: all; }
  .profile-elo { font-family: 'Orbitron', monospace; font-size: 12px; font-weight: 700; color: var(--wind-cyan); padding: 2px 8px; background: rgba(34, 211, 238, 0.1); border-radius: 4px; }
  .profile-record { font-size: 11px; color: var(--text-muted); }
  .profile-streak { font-size: 10px; font-weight: 700; padding: 2px 6px; border-radius: 3px; }
  .profile-streak.win-streak { color: #22c55e; background: rgba(34, 197, 94, 0.1); }
  .profile-streak.loss-streak { color: #ef4444; background: rgba(239, 68, 68, 0.1); }

  /* Connection Quality Overlay */
  .quality-overlay {
    position: fixed;
    top: 16px;
    right: 16px;
    width: 180px;
    background: rgba(10, 15, 20, 0.92);
    border: 1px solid rgba(34, 211, 238, 0.25);
    border-radius: 10px;
    padding: 10px 14px;
    z-index: 1000;
    backdrop-filter: blur(8px);
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.5);
  }
  .overlay-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 8px;
    padding-bottom: 6px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  }
  .overlay-title {
    font-family: 'Orbitron', monospace;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 2px;
    color: var(--wind-cyan);
  }
  .overlay-close {
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 12px;
    cursor: pointer;
    padding: 0 4px;
  }
  .overlay-close:hover { color: var(--text-primary); }
  .overlay-stats { display: flex; flex-direction: column; gap: 4px; }
  .stat-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 11px;
  }
  .stat-label {
    font-family: 'Orbitron', monospace;
    font-size: 9px;
    letter-spacing: 1px;
    color: var(--text-muted);
  }
  .stat-value {
    font-family: 'Orbitron', monospace;
    font-size: 12px;
    font-weight: 700;
    color: var(--text-primary);
  }
  .stat-good { color: #22c55e; }
  .stat-warn { color: #f59e0b; }
  .stat-bad { color: #ef4444; }
  .desync-warning {
    margin-top: 4px;
    padding-top: 4px;
    border-top: 1px solid rgba(239, 68, 68, 0.3);
    animation: pulse-red 1s ease-in-out infinite;
  }
  @keyframes pulse-red {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.5; }
  }
</style>
