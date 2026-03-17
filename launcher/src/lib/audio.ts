/**
 * HowlingWind Audio System
 *
 * Manages UI sounds + ambient audio, auto-ducks when game is running.
 * Uses Web Audio API for low-latency playback and volume control.
 */

let audioContext: AudioContext | null = null;
let masterGain: GainNode | null = null;
let ambientGain: GainNode | null = null;
let sfxGain: GainNode | null = null;

// State
let gameRunning = false;
let ambientSource: AudioBufferSourceNode | null = null;
let ambientBuffer: AudioBuffer | null = null;
let muted = false;

// Volume levels (0-1)
const MASTER_VOLUME = 0.7;
const AMBIENT_VOLUME = 0.3;
const SFX_VOLUME = 0.5;
const DUCKED_VOLUME = 0.0; // Completely mute UI audio during gameplay

/**
 * Initialize the audio system. Call once on app mount.
 */
export function initAudio(): void {
  if (audioContext) return;

  audioContext = new AudioContext();

  // Master gain
  masterGain = audioContext.createGain();
  masterGain.gain.value = MASTER_VOLUME;
  masterGain.connect(audioContext.destination);

  // Ambient channel (wind sounds)
  ambientGain = audioContext.createGain();
  ambientGain.gain.value = AMBIENT_VOLUME;
  ambientGain.connect(masterGain);

  // SFX channel (button clicks, notifications)
  sfxGain = audioContext.createGain();
  sfxGain.gain.value = SFX_VOLUME;
  sfxGain.connect(masterGain);
}

/**
 * Resume audio context (needed after user interaction on some browsers).
 */
export async function resumeAudio(): Promise<void> {
  if (audioContext?.state === "suspended") {
    await audioContext.resume();
  }
}

// ── Ambient Wind Sound ──

/**
 * Play a short wind gust on app startup — dramatic entrance, then fades.
 * Followed by chill ambient background music.
 */
export function startAmbientWind(): void {
  if (!audioContext || !ambientGain) return;
  if (ambientSource) return; // Already played

  // Short wind gust (3 seconds, fades out)
  const gustDuration = 3;
  const bufferSize = Math.ceil(audioContext.sampleRate * gustDuration);
  const buffer = audioContext.createBuffer(2, bufferSize, audioContext.sampleRate);

  for (let channel = 0; channel < 2; channel++) {
    const data = buffer.getChannelData(channel);
    let lastOut = 0;
    for (let i = 0; i < bufferSize; i++) {
      const white = Math.random() * 2 - 1;
      lastOut = (lastOut + 0.02 * white) / 1.02;
      // Envelope: quick rise, slow fade
      const t = i / bufferSize;
      const envelope = t < 0.15 ? t / 0.15 : Math.pow(1 - (t - 0.15) / 0.85, 2);
      data[i] = lastOut * 4.0 * envelope;
    }
  }

  const gustSource = audioContext.createBufferSource();
  gustSource.buffer = buffer;

  const lowpass = audioContext.createBiquadFilter();
  lowpass.type = "lowpass";
  lowpass.frequency.value = 500;
  lowpass.Q.value = 0.3;

  gustSource.connect(lowpass);
  lowpass.connect(ambientGain);

  ambientGain.gain.setValueAtTime(0, audioContext.currentTime);
  ambientGain.gain.linearRampToValueAtTime(0.5, audioContext.currentTime + 0.4);
  ambientGain.gain.linearRampToValueAtTime(0, audioContext.currentTime + gustDuration);

  gustSource.start();
  ambientSource = gustSource; // Track it

  // After the gust, start chill background music
  setTimeout(() => startBackgroundMusic(), gustDuration * 1000);
}

/**
 * Chill lo-fi style background music using synthesized chords.
 */
function startBackgroundMusic(): void {
  if (!audioContext || !ambientGain || gameRunning) return;

  // Smooth pad chord progression — chill vibes
  const chords = [
    [261.6, 329.6, 392.0], // C major
    [220.0, 277.2, 329.6], // Am
    [246.9, 311.1, 370.0], // Bm-ish
    [196.0, 246.9, 293.7], // G
  ];

  let chordIndex = 0;

  function playChord() {
    if (!audioContext || !ambientGain || gameRunning || muted) return;

    const chord = chords[chordIndex % chords.length];
    chordIndex++;

    for (const freq of chord) {
      const osc = audioContext.createOscillator();
      const gain = audioContext.createGain();

      osc.type = "sine";
      osc.frequency.value = freq;

      // Soft pad envelope: slow attack, long sustain, slow release
      const now = audioContext.currentTime;
      gain.gain.setValueAtTime(0, now);
      gain.gain.linearRampToValueAtTime(0.04, now + 1.0);
      gain.gain.setValueAtTime(0.04, now + 3.0);
      gain.gain.linearRampToValueAtTime(0, now + 4.5);

      osc.connect(gain);
      gain.connect(ambientGain);
      osc.start(now);
      osc.stop(now + 5);
    }

    // Next chord every 4.5 seconds
    bgMusicTimer = window.setTimeout(playChord, 4500);
  }

  ambientGain.gain.setValueAtTime(0, audioContext.currentTime);
  ambientGain.gain.linearRampToValueAtTime(AMBIENT_VOLUME, audioContext.currentTime + 2);
  playChord();
}

let bgMusicTimer: number | null = null;

/**
 * Stop all ambient audio.
 */
export function stopAmbientWind(): void {
  if (bgMusicTimer) {
    clearTimeout(bgMusicTimer);
    bgMusicTimer = null;
  }
  if (!audioContext || !ambientGain) return;

  ambientGain.gain.linearRampToValueAtTime(0, audioContext.currentTime + 1.0);

  if (ambientSource) {
    const src = ambientSource;
    setTimeout(() => {
      try { src.stop(); } catch {}
    }, 1000);
    ambientSource = null;
  }
}

// ── Sound Effects ──

type SfxName =
  | "click"
  | "hover"
  | "match_found"
  | "match_start"
  | "match_end"
  | "elo_up"
  | "elo_down"
  | "error"
  | "whoosh";

/**
 * Play a synthesized sound effect. No audio files needed.
 */
export function playSfx(name: SfxName): void {
  if (!audioContext || !sfxGain || muted || gameRunning) return;

  resumeAudio();

  switch (name) {
    case "click":
      playTone(800, 0.05, "square", 0.3);
      break;
    case "hover":
      playTone(600, 0.03, "sine", 0.1);
      break;
    case "match_found":
      // Rising two-tone ping
      playTone(523, 0.15, "sine", 0.4);
      setTimeout(() => playTone(784, 0.2, "sine", 0.4), 150);
      break;
    case "match_start":
      // Three ascending tones
      playTone(440, 0.12, "square", 0.3);
      setTimeout(() => playTone(554, 0.12, "square", 0.3), 150);
      setTimeout(() => playTone(659, 0.2, "square", 0.4), 300);
      break;
    case "match_end":
      // Descending tones
      playTone(659, 0.15, "sine", 0.4);
      setTimeout(() => playTone(523, 0.15, "sine", 0.4), 200);
      setTimeout(() => playTone(392, 0.3, "sine", 0.3), 400);
      break;
    case "elo_up":
      // Cheerful rising arpeggio
      playTone(523, 0.1, "sine", 0.3);
      setTimeout(() => playTone(659, 0.1, "sine", 0.3), 80);
      setTimeout(() => playTone(784, 0.15, "sine", 0.4), 160);
      break;
    case "elo_down":
      // Sad descending
      playTone(392, 0.15, "sine", 0.25);
      setTimeout(() => playTone(330, 0.2, "sine", 0.2), 150);
      break;
    case "error":
      // Buzz
      playTone(200, 0.15, "sawtooth", 0.3);
      setTimeout(() => playTone(180, 0.2, "sawtooth", 0.3), 100);
      break;
    case "whoosh":
      playNoise(0.3, 200, 2000);
      break;
  }
}

function playTone(
  freq: number,
  duration: number,
  type: OscillatorType,
  volume: number
): void {
  if (!audioContext || !sfxGain) return;

  const osc = audioContext.createOscillator();
  const gain = audioContext.createGain();

  osc.type = type;
  osc.frequency.value = freq;
  gain.gain.setValueAtTime(volume, audioContext.currentTime);
  gain.gain.exponentialRampToValueAtTime(0.001, audioContext.currentTime + duration);

  osc.connect(gain);
  gain.connect(sfxGain);
  osc.start();
  osc.stop(audioContext.currentTime + duration + 0.05);
}

function playNoise(duration: number, lowFreq: number, highFreq: number): void {
  if (!audioContext || !sfxGain) return;

  const bufferSize = Math.ceil(audioContext.sampleRate * duration);
  const buffer = audioContext.createBuffer(1, bufferSize, audioContext.sampleRate);
  const data = buffer.getChannelData(0);

  for (let i = 0; i < bufferSize; i++) {
    data[i] = Math.random() * 2 - 1;
  }

  const source = audioContext.createBufferSource();
  source.buffer = buffer;

  const bandpass = audioContext.createBiquadFilter();
  bandpass.type = "bandpass";
  bandpass.frequency.value = (lowFreq + highFreq) / 2;
  bandpass.Q.value = 0.5;

  const gain = audioContext.createGain();
  gain.gain.setValueAtTime(0.3, audioContext.currentTime);
  gain.gain.exponentialRampToValueAtTime(0.001, audioContext.currentTime + duration);

  source.connect(bandpass);
  bandpass.connect(gain);
  gain.connect(sfxGain);
  source.start();
}

// ── Game Audio Ducking ──

/**
 * Call when Dolphin/game starts. Mutes UI audio.
 */
export function onGameStart(): void {
  gameRunning = true;
  if (!audioContext || !masterGain) return;

  // Fade out all UI audio smoothly
  masterGain.gain.linearRampToValueAtTime(DUCKED_VOLUME, audioContext.currentTime + 0.5);
}

/**
 * Call when game exits. Restores UI audio.
 */
export function onGameEnd(): void {
  gameRunning = false;
  if (!audioContext || !masterGain) return;

  // Fade back in
  masterGain.gain.linearRampToValueAtTime(MASTER_VOLUME, audioContext.currentTime + 0.8);
}

/**
 * Temporarily un-duck for important notifications during gameplay
 * (e.g., match results overlay).
 */
export function temporaryUnduck(durationMs: number = 3000): void {
  if (!audioContext || !masterGain || !gameRunning) return;

  masterGain.gain.linearRampToValueAtTime(0.4, audioContext.currentTime + 0.3);
  setTimeout(() => {
    if (gameRunning && masterGain && audioContext) {
      masterGain.gain.linearRampToValueAtTime(DUCKED_VOLUME, audioContext.currentTime + 0.5);
    }
  }, durationMs);
}

// ── Global Controls ──

export function setMuted(m: boolean): void {
  muted = m;
  if (masterGain && audioContext) {
    masterGain.gain.linearRampToValueAtTime(
      m ? 0 : MASTER_VOLUME,
      audioContext.currentTime + 0.2
    );
  }
}

export function isMuted(): boolean {
  return muted;
}
