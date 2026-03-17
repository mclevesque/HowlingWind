<script lang="ts">
  /**
   * VoiceChat — lightweight P2P voice chat widget.
   * Uses WebRTC for audio, Firebase for signaling.
   * Renders as a floating mini-panel that persists during gameplay.
   */

  let isMuted = $state(false);
  let isConnected = $state(false);
  let isConnecting = $state(false);
  let peerName = $state("Opponent");
  let volume = $state(80);
  let micDevices = $state<MediaDeviceInfo[]>([]);
  let selectedMic = $state("");
  let localStream: MediaStream | null = null;
  let peerConnection: RTCPeerConnection | null = null;
  let remoteAudio: HTMLAudioElement | null = null;
  let micLevel = $state(0);
  let peerLevel = $state(0);
  let analyserNode: AnalyserNode | null = null;
  let micLevelInterval: number | null = null;
  let expanded = $state(false);

  // Props
  let {
    roomId = "",
    playerId = "",
    visible = false,
    onClose = () => {},
  }: {
    roomId?: string;
    playerId?: string;
    visible?: boolean;
    onClose?: () => void;
  } = $props();

  // Auto-detect microphones on mount
  $effect(() => {
    if (visible) {
      enumerateMics();
    }
  });

  // Connect when roomId changes and visible
  $effect(() => {
    if (visible && roomId && playerId) {
      connectVoice();
    }
    return () => {
      disconnectVoice();
    };
  });

  async function enumerateMics() {
    try {
      // Request permission first (needed to get device labels)
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      stream.getTracks().forEach(t => t.stop());

      const devices = await navigator.mediaDevices.enumerateDevices();
      micDevices = devices.filter(d => d.kind === "audioinput");
      if (micDevices.length > 0 && !selectedMic) {
        selectedMic = micDevices[0].deviceId;
      }
    } catch (e) {
      console.warn("[voicechat] Mic access denied:", e);
    }
  }

  async function connectVoice() {
    if (isConnected || isConnecting) return;
    isConnecting = true;

    try {
      // Get local audio stream with noise suppression
      const constraints: MediaStreamConstraints = {
        audio: {
          deviceId: selectedMic ? { exact: selectedMic } : undefined,
          echoCancellation: true,
          noiseSuppression: true,
          autoGainControl: true,
          sampleRate: 48000,
        }
      };
      localStream = await navigator.mediaDevices.getUserMedia(constraints);

      // Set up audio level monitoring
      const audioCtx = new AudioContext();
      const source = audioCtx.createMediaStreamSource(localStream);
      analyserNode = audioCtx.createAnalyser();
      analyserNode.fftSize = 256;
      source.connect(analyserNode);
      startMicLevelMonitor();

      // Create WebRTC peer connection
      peerConnection = new RTCPeerConnection({
        iceServers: [
          { urls: "stun:stun.l.google.com:19302" },
          { urls: "stun:stun1.l.google.com:19302" },
        ]
      });

      // Add local audio tracks
      localStream.getAudioTracks().forEach(track => {
        peerConnection!.addTrack(track, localStream!);
      });

      // Handle remote audio
      peerConnection.ontrack = (event) => {
        if (!remoteAudio) {
          remoteAudio = new Audio();
          remoteAudio.autoplay = true;
        }
        remoteAudio.srcObject = event.streams[0];
        remoteAudio.volume = volume / 100;
        isConnected = true;
        isConnecting = false;
      };

      // ICE candidates — send via Firebase signaling
      peerConnection.onicecandidate = (event) => {
        if (event.candidate && roomId) {
          sendVoiceSignal(roomId, playerId, {
            type: "ice",
            candidate: event.candidate.toJSON(),
          });
        }
      };

      peerConnection.onconnectionstatechange = () => {
        const state = peerConnection?.connectionState;
        if (state === "connected") {
          isConnected = true;
          isConnecting = false;
        } else if (state === "disconnected" || state === "failed" || state === "closed") {
          isConnected = false;
          isConnecting = false;
        }
      };

      // Create offer (initiator is the host / lower playerId)
      // Both peers create offers; the one that arrives first wins
      const offer = await peerConnection.createOffer();
      await peerConnection.setLocalDescription(offer);
      sendVoiceSignal(roomId, playerId, {
        type: "voice_offer",
        sdp: offer.sdp,
      });

      // Listen for voice signals from peer
      listenForVoiceSignals();

    } catch (e) {
      console.error("[voicechat] Connect failed:", e);
      isConnecting = false;
    }
  }

  function disconnectVoice() {
    stopMicLevelMonitor();
    if (localStream) {
      localStream.getTracks().forEach(t => t.stop());
      localStream = null;
    }
    if (peerConnection) {
      peerConnection.close();
      peerConnection = null;
    }
    if (remoteAudio) {
      remoteAudio.srcObject = null;
      remoteAudio = null;
    }
    isConnected = false;
    isConnecting = false;
  }

  function toggleMute() {
    isMuted = !isMuted;
    if (localStream) {
      localStream.getAudioTracks().forEach(t => {
        t.enabled = !isMuted;
      });
    }
  }

  function updateVolume() {
    if (remoteAudio) {
      remoteAudio.volume = volume / 100;
    }
  }

  function startMicLevelMonitor() {
    if (micLevelInterval) return;
    const dataArray = new Uint8Array(analyserNode!.frequencyBinCount);
    micLevelInterval = window.setInterval(() => {
      if (analyserNode) {
        analyserNode.getByteFrequencyData(dataArray);
        const avg = dataArray.reduce((a, b) => a + b, 0) / dataArray.length;
        micLevel = Math.min(100, avg * 1.5);
      }
    }, 100);
  }

  function stopMicLevelMonitor() {
    if (micLevelInterval) {
      clearInterval(micLevelInterval);
      micLevelInterval = null;
    }
    micLevel = 0;
  }

  // Firebase voice signaling (reuse existing signaling infrastructure)
  async function sendVoiceSignal(room: string, from: string, signal: any) {
    try {
      const { getDatabase, ref, push, set } = await import("firebase/database");
      const { db } = await import("./firebase");
      if (!db) return;
      const sigRef = push(ref(db, `voice_signals/${room}`));
      await set(sigRef, { from, signal, timestamp: Date.now() });
    } catch (e) {
      console.warn("[voicechat] Signal send failed:", e);
    }
  }

  async function listenForVoiceSignals() {
    try {
      const { getDatabase, ref, onValue, remove } = await import("firebase/database");
      const { db } = await import("./firebase");
      if (!db) return;

      onValue(ref(db, `voice_signals/${roomId}`), async (snapshot) => {
        const signals = snapshot.val();
        if (!signals) return;

        for (const [key, data] of Object.entries(signals) as [string, any][]) {
          if (data.from === playerId) continue; // Skip our own signals

          const signal = data.signal;
          try {
            if (signal.type === "voice_offer" && peerConnection) {
              await peerConnection.setRemoteDescription(
                new RTCSessionDescription({ type: "offer", sdp: signal.sdp })
              );
              const answer = await peerConnection.createAnswer();
              await peerConnection.setLocalDescription(answer);
              sendVoiceSignal(roomId, playerId, {
                type: "voice_answer",
                sdp: answer.sdp,
              });
            } else if (signal.type === "voice_answer" && peerConnection) {
              await peerConnection.setRemoteDescription(
                new RTCSessionDescription({ type: "answer", sdp: signal.sdp })
              );
            } else if (signal.type === "ice" && peerConnection) {
              await peerConnection.addIceCandidate(new RTCIceCandidate(signal.candidate));
            }
          } catch (e) {
            console.warn("[voicechat] Signal processing error:", e);
          }

          // Clean up processed signal
          remove(ref(db, `voice_signals/${roomId}/${key}`));
        }
      });
    } catch (e) {
      console.warn("[voicechat] Signal listen failed:", e);
    }
  }
</script>

{#if visible}
  <div class="voice-widget" class:expanded>
    <div class="voice-header" onclick={() => expanded = !expanded}>
      <div class="voice-status">
        <span class="voice-dot" class:connected={isConnected} class:connecting={isConnecting}></span>
        <span class="voice-label">
          {isConnected ? "VOICE" : isConnecting ? "CONNECTING..." : "VOICE OFF"}
        </span>
      </div>
      <div class="voice-controls-mini">
        <button class="voice-btn-mini" class:muted={isMuted} onclick={(e) => { e.stopPropagation(); toggleMute(); }} title={isMuted ? "Unmute" : "Mute"}>
          {#if isMuted}
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <line x1="1" y1="1" x2="23" y2="23"/><path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6"/>
              <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2c0 .34-.03.67-.08 1"/>
              <line x1="12" y1="19" x2="12" y2="23"/><line x1="8" y1="23" x2="16" y2="23"/>
            </svg>
          {:else}
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/>
              <path d="M19 10v2a7 7 0 0 1-14 0v-2"/>
              <line x1="12" y1="19" x2="12" y2="23"/><line x1="8" y1="23" x2="16" y2="23"/>
            </svg>
          {/if}
        </button>
        {#if isConnected && !isMuted}
          <div class="mic-level-bar">
            <div class="mic-level-fill" style="width: {micLevel}%"></div>
          </div>
        {/if}
      </div>
    </div>

    {#if expanded}
      <div class="voice-body">
        {#if micDevices.length > 1}
          <div class="voice-row">
            <label class="voice-label-sm">Mic</label>
            <select class="voice-select" bind:value={selectedMic} onchange={() => { disconnectVoice(); connectVoice(); }}>
              {#each micDevices as mic}
                <option value={mic.deviceId}>{mic.label || `Mic ${mic.deviceId.slice(0,8)}`}</option>
              {/each}
            </select>
          </div>
        {/if}
        <div class="voice-row">
          <label class="voice-label-sm">Vol</label>
          <input type="range" min="0" max="100" bind:value={volume} oninput={updateVolume} class="voice-slider" />
          <span class="voice-vol-num">{volume}</span>
        </div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .voice-widget {
    position: fixed;
    bottom: 16px;
    right: 16px;
    z-index: 99998;
    background: rgba(10, 10, 18, 0.95);
    border: 1px solid rgba(34, 211, 238, 0.2);
    border-radius: 10px;
    backdrop-filter: blur(12px);
    min-width: 160px;
    font-family: 'Orbitron', monospace;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.5);
    transition: all 0.2s ease;
  }
  .voice-widget.expanded {
    min-width: 220px;
  }
  .voice-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 12px;
    cursor: pointer;
    gap: 8px;
  }
  .voice-status {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .voice-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #444;
    transition: background 0.3s;
  }
  .voice-dot.connected {
    background: #22c55e;
    box-shadow: 0 0 6px rgba(34, 197, 94, 0.5);
  }
  .voice-dot.connecting {
    background: #eab308;
    animation: blink 1s ease-in-out infinite;
  }
  @keyframes blink {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.3; }
  }
  .voice-label {
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 1.5px;
    color: #888;
  }
  .voice-controls-mini {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .voice-btn-mini {
    width: 28px;
    height: 28px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(255, 255, 255, 0.05);
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 6px;
    color: #aaa;
    cursor: pointer;
    transition: all 0.15s;
  }
  .voice-btn-mini:hover {
    background: rgba(255, 255, 255, 0.1);
    color: #fff;
  }
  .voice-btn-mini.muted {
    color: #ef4444;
    border-color: rgba(239, 68, 68, 0.3);
  }
  .mic-level-bar {
    width: 40px;
    height: 4px;
    background: rgba(255, 255, 255, 0.1);
    border-radius: 2px;
    overflow: hidden;
  }
  .mic-level-fill {
    height: 100%;
    background: linear-gradient(90deg, #22c55e, #22d3ee);
    border-radius: 2px;
    transition: width 0.1s;
  }

  /* Expanded body */
  .voice-body {
    padding: 4px 12px 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    border-top: 1px solid rgba(255, 255, 255, 0.05);
  }
  .voice-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .voice-label-sm {
    font-size: 8px;
    font-weight: 600;
    letter-spacing: 1px;
    color: #666;
    min-width: 24px;
  }
  .voice-select {
    flex: 1;
    background: rgba(255, 255, 255, 0.05);
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 4px;
    color: #ccc;
    font-size: 10px;
    padding: 3px 6px;
    font-family: inherit;
  }
  .voice-slider {
    flex: 1;
    height: 4px;
    -webkit-appearance: none;
    appearance: none;
    background: rgba(255, 255, 255, 0.1);
    border-radius: 2px;
    outline: none;
  }
  .voice-slider::-webkit-slider-thumb {
    -webkit-appearance: none;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: var(--wind-cyan, #22d3ee);
    cursor: pointer;
  }
  .voice-vol-num {
    font-size: 9px;
    color: #666;
    min-width: 20px;
    text-align: right;
  }
</style>
