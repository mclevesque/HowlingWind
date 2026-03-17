<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  interface GCPadMapping {
    device: string;
    a: string; b: string; x: string; y: string; z: string; start: string;
    l: string; r: string;
    stick_up: string; stick_down: string; stick_left: string; stick_right: string;
    cstick_up: string; cstick_down: string; cstick_left: string; cstick_right: string;
    dpad_up: string; dpad_down: string; dpad_left: string; dpad_right: string;
  }

  // ── Port type options (matches Dolphin exactly) ──
  const PORT_TYPES = [
    "None",
    "Standard Controller",
    "GameCube Adapter for Wii U",
    "Steering Wheel",
    "Dance Mat",
    "DK Bongos",
    "GBA",
    "Keyboard",
  ];

  let portTypes = $state(["Standard Controller", "None", "None", "None"]);
  let configuringPort: number | null = $state(null);
  let mapping: GCPadMapping = $state(getKeyboardDefaults());
  let listening: string | null = $state(null);
  let saved = $state(false);
  let loading = $state(false);
  let selectedPreset = $state("keyboard");
  let customDevice = $state("");
  let showCustom = $state(false);
  let statusMsg = $state("");
  let dolphinDevices: string[] = $state([]);
  let pollInterval: ReturnType<typeof setInterval> | null = null;
  let baselineButtons: boolean[] = $state([]);
  let baselineAxes: number[] = $state([]);
  let showManualPicker = $state(false);

  // ── Device presets (every Dolphin backend) ──
  const DEVICE_PRESETS = [
    { group: "Keyboard", options: [
      { label: "Keyboard / Mouse", value: "DInput/0/Keyboard Mouse" },
    ]},
    { group: "SDL (Xbox / PlayStation / Switch / Generic)", options: [
      { label: "Xbox One S Controller", value: "SDL/0/Xbox One S Controller" },
      { label: "Xbox Wireless Controller", value: "SDL/0/Xbox Wireless Controller" },
      { label: "Xbox 360 Controller", value: "SDL/0/Xbox 360 Controller" },
      { label: "Xbox Series X Controller", value: "SDL/0/Xbox Series X Controller" },
      { label: "PS4 Controller (DualShock 4)", value: "SDL/0/Sony Interactive Entertainment Wireless Controller" },
      { label: "PS5 Controller (DualSense)", value: "SDL/0/DualSense Wireless Controller" },
      { label: "Switch Pro Controller", value: "SDL/0/Nintendo Switch Pro Controller" },
      { label: "Generic SDL Controller", value: "SDL/0/Gamepad" },
      { label: "SDL Controller (Port 2)", value: "SDL/1/Gamepad" },
      { label: "SDL Controller (Port 3)", value: "SDL/2/Gamepad" },
      { label: "SDL Controller (Port 4)", value: "SDL/3/Gamepad" },
    ]},
    { group: "XInput (Wired Xbox / Adapter)", options: [
      { label: "XInput Gamepad 1", value: "XInput/0/Gamepad" },
      { label: "XInput Gamepad 2", value: "XInput/1/Gamepad" },
      { label: "XInput Gamepad 3", value: "XInput/2/Gamepad" },
      { label: "XInput Gamepad 4", value: "XInput/3/Gamepad" },
    ]},
    { group: "DInput (Legacy / DirectInput)", options: [
      { label: "DInput Controller 1", value: "DInput/0/Gamepad" },
      { label: "DInput Controller 2", value: "DInput/1/Gamepad" },
    ]},
    { group: "GameCube Adapter", options: [
      { label: "GC Adapter Port 1", value: "GCAdapter/0/GameCube Controller" },
      { label: "GC Adapter Port 2", value: "GCAdapter/1/GameCube Controller" },
      { label: "GC Adapter Port 3", value: "GCAdapter/2/GameCube Controller" },
      { label: "GC Adapter Port 4", value: "GCAdapter/3/GameCube Controller" },
    ]},
  ];

  // ── All Dolphin input options ──
  const DOLPHIN_INPUTS = [
    { group: "Face Buttons", options: [
      { label: "A (South)", value: "Button S" },
      { label: "B (East)", value: "Button E" },
      { label: "X (West)", value: "Button W" },
      { label: "Y (North)", value: "Button N" },
    ]},
    { group: "Shoulders & Triggers", options: [
      { label: "LB / L1", value: "Shoulder L" },
      { label: "RB / R1", value: "Shoulder R" },
      { label: "LT / L2", value: "Trigger L" },
      { label: "RT / R2", value: "Trigger R" },
    ]},
    { group: "Menu", options: [
      { label: "Start", value: "Start" },
      { label: "Back", value: "Back" },
      { label: "Guide", value: "Guide" },
      { label: "LS Click", value: "Thumb L" },
      { label: "RS Click", value: "Thumb R" },
    ]},
    { group: "D-Pad", options: [
      { label: "D-Up", value: "Pad N" },
      { label: "D-Down", value: "Pad S" },
      { label: "D-Left", value: "Pad W" },
      { label: "D-Right", value: "Pad E" },
    ]},
    { group: "Left Stick", options: [
      { label: "LS Up", value: "Left Y+" },
      { label: "LS Down", value: "Left Y-" },
      { label: "LS Left", value: "Left X-" },
      { label: "LS Right", value: "Left X+" },
    ]},
    { group: "Right Stick", options: [
      { label: "RS Up", value: "Right Y+" },
      { label: "RS Down", value: "Right Y-" },
      { label: "RS Left", value: "Right X-" },
      { label: "RS Right", value: "Right X+" },
    ]},
    { group: "Full Axis", options: [
      { label: "Full LX", value: "Full Axis 0+" },
      { label: "Full LY", value: "Full Axis 1+" },
      { label: "Full RX", value: "Full Axis 2+" },
      { label: "Full RY", value: "Full Axis 3+" },
      { label: "Full LT", value: "Full Axis 4+" },
      { label: "Full RT", value: "Full Axis 5+" },
    ]},
  ];

  const INPUT_DISPLAY: Record<string, string> = {};
  DOLPHIN_INPUTS.forEach(g => g.options.forEach(o => { INPUT_DISPLAY[o.value] = o.label; }));

  const KEY_DISPLAY: Record<string, string> = {
    "RETURN": "Enter", "UP": "\u2191", "DOWN": "\u2193", "LEFT": "\u2190", "RIGHT": "\u2192",
    "SPACE": "Space", "BACK": "Bksp", "TAB": "Tab", "ESCAPE": "Esc",
  };

  function displayKey(key: string): string {
    if (isGamepad()) return INPUT_DISPLAY[key] || key;
    return KEY_DISPLAY[key] || key;
  }

  function isGamepad(): boolean {
    return selectedPreset !== "keyboard";
  }

  // ── Fetch real devices from Dolphin ──
  async function fetchDolphinDevices() {
    try {
      dolphinDevices = await invoke("get_dolphin_devices") as string[];
    } catch {
      dolphinDevices = [];
    }
  }
  fetchDolphinDevices();

  // ── Configure port ──
  function openConfigure(port: number) {
    if (portTypes[port] === "None") return;
    configuringPort = port;
    fetchDolphinDevices(); // Refresh device list
    loadMapping(port + 1); // Dolphin pads are 1-indexed
  }

  function closeConfigure() {
    configuringPort = null;
    listening = null;
  }

  // ── Device change ──
  function onDeviceChange() {
    if (selectedPreset === "keyboard") {
      mapping = getKeyboardDefaults();
      showCustom = false;
    } else if (selectedPreset === "custom") {
      showCustom = true;
    } else {
      showCustom = false;
      mapping = getGamepadDefaults(selectedPreset);
    }
  }

  // ── Defaults ──
  function getKeyboardDefaults(): GCPadMapping {
    return {
      device: "DInput/0/Keyboard Mouse",
      a: "X", b: "Z", x: "C", y: "S", z: "D", start: "RETURN",
      l: "Q", r: "W",
      stick_up: "UP", stick_down: "DOWN", stick_left: "LEFT", stick_right: "RIGHT",
      cstick_up: "I", cstick_down: "K", cstick_left: "J", cstick_right: "L",
      dpad_up: "T", dpad_down: "G", dpad_left: "F", dpad_right: "H",
    };
  }

  function getGamepadDefaults(deviceString: string): GCPadMapping {
    if (deviceString.includes("GCAdapter")) {
      return {
        device: deviceString,
        a: "Button A", b: "Button B", x: "Button X", y: "Button Y",
        z: "Button Z", start: "Button Start", l: "L Analog", r: "R Analog",
        stick_up: "Main Stick/Up", stick_down: "Main Stick/Down",
        stick_left: "Main Stick/Left", stick_right: "Main Stick/Right",
        cstick_up: "C Stick/Up", cstick_down: "C Stick/Down",
        cstick_left: "C Stick/Left", cstick_right: "C Stick/Right",
        dpad_up: "Pad N", dpad_down: "Pad S", dpad_left: "Pad W", dpad_right: "Pad E",
      };
    }
    return {
      device: deviceString,
      a: "Button S", b: "Button E", x: "Button W", y: "Button N",
      z: "Trigger R", start: "Start", l: "Trigger L", r: "Shoulder R",
      stick_up: "Left Y+", stick_down: "Left Y-", stick_left: "Left X-", stick_right: "Left X+",
      cstick_up: "Right Y+", cstick_down: "Right Y-", cstick_left: "Right X-", cstick_right: "Right X+",
      dpad_up: "Pad N", dpad_down: "Pad S", dpad_left: "Pad W", dpad_right: "Pad E",
    };
  }

  // ── Rebinding ──

  // XInput button index → SDL button name mapping
  const XINPUT_TO_SDL: Record<number, string> = {
    0: "Button S",      // A (south)
    1: "Button E",      // B (east)
    2: "Button W",      // X (west)
    3: "Button N",      // Y (north)
    4: "Shoulder L",    // LB
    5: "Shoulder R",    // RB
    6: "Trigger L",     // LT
    7: "Trigger R",     // RT
    8: "Back",          // Back
    9: "Start",         // Start
    10: "Thumb L",      // LS click
    11: "Thumb R",      // RS click
    12: "Pad N",        // D-Up
    13: "Pad S",        // D-Down
    14: "Pad W",        // D-Left
    15: "Pad E",        // D-Right
  };

  // Axis index + direction → SDL axis name
  const AXIS_TO_SDL: Record<string, string> = {
    "0+": "Left X+",   "0-": "Left X-",
    "1+": "Left Y+",   "1-": "Left Y-",
    "2+": "Right X+",  "2-": "Right X-",
    "3+": "Right Y+",  "3-": "Right Y-",
    "4+": "Trigger L",  // LT as axis
    "5+": "Trigger R",  // RT as axis
  };

  function startListening(button: string) {
    listening = button;
    if (isGamepad()) {
      startGamepadPolling();
    }
  }

  function stopListening() {
    listening = null;
    stopGamepadPolling();
  }

  function pickInput(value: string) {
    if (listening) {
      (mapping as any)[listening] = value;
      stopListening();
    }
  }

  function startGamepadPolling() {
    stopGamepadPolling();
    // Capture baseline state first (what's currently pressed)
    invoke("poll_gamepad", { index: 0 }).then((state: any) => {
      if (state.connected) {
        baselineButtons = [...state.buttons];
        baselineAxes = [...state.axes];
      }
    });

    // Small delay before polling to let user release any current buttons
    setTimeout(() => {
      pollInterval = setInterval(async () => {
        if (!listening) { stopGamepadPolling(); return; }
        try {
          const state: any = await invoke("poll_gamepad", { index: 0 });
          if (!state.connected) return;

          // Check for newly pressed buttons (not in baseline)
          for (let i = 0; i < state.buttons.length; i++) {
            if (state.buttons[i] && !baselineButtons[i]) {
              const sdlName = XINPUT_TO_SDL[i];
              if (sdlName) {
                pickInput(sdlName);
                return;
              }
            }
          }

          // Check for axis movement (threshold 0.5, must be new movement)
          const axisThreshold = 0.5;
          const axisDeadzone = 0.3;
          for (let i = 0; i < state.axes.length && i < 6; i++) {
            const current = state.axes[i];
            const baseline = baselineAxes[i] || 0;
            // Axis newly moved past threshold
            if (Math.abs(current) > axisThreshold && Math.abs(baseline) < axisDeadzone) {
              const dir = current > 0 ? "+" : "-";
              const sdlName = AXIS_TO_SDL[`${i}${dir}`];
              if (sdlName) {
                pickInput(sdlName);
                return;
              }
            }
          }
        } catch {}
      }, 50); // Poll at 20Hz
    }, 200); // 200ms grace period
  }

  function stopGamepadPolling() {
    if (pollInterval) {
      clearInterval(pollInterval);
      pollInterval = null;
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (!listening || isGamepad()) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.code === "Escape") { stopListening(); return; }
    let key = mapBrowserKeyToDolphin(e.code, e.key);
    if (!key) return;
    (mapping as any)[listening] = key;
    stopListening();
  }

  function mapBrowserKeyToDolphin(code: string, key: string): string {
    if (code.startsWith("Key")) return code.replace("Key", "");
    if (code.startsWith("Digit")) return code.replace("Digit", "");
    if (code === "ArrowUp") return "UP";
    if (code === "ArrowDown") return "DOWN";
    if (code === "ArrowLeft") return "LEFT";
    if (code === "ArrowRight") return "RIGHT";
    if (code === "Enter") return "RETURN";
    if (code === "Space") return "SPACE";
    if (code === "ShiftLeft" || code === "ShiftRight") return "Shift";
    if (code === "ControlLeft" || code === "ControlRight") return "Ctrl";
    if (code === "Backspace") return "BACK";
    if (code === "Tab") return "TAB";
    if (code === "Escape") return "ESCAPE";
    return key.toUpperCase();
  }

  // ── Save/Load ──
  async function loadMapping(pad: number) {
    loading = true;
    try {
      mapping = await invoke("get_gcpad_mapping", { pad }) as GCPadMapping;
      const allOptions = DEVICE_PRESETS.flatMap(g => g.options);
      const match = allOptions.find(o => o.value === mapping.device);
      if (match) {
        selectedPreset = match.value === "DInput/0/Keyboard Mouse" ? "keyboard" : match.value;
        showCustom = false;
      } else if (mapping.device && mapping.device !== "DInput/0/Keyboard Mouse") {
        selectedPreset = "custom";
        customDevice = mapping.device;
        showCustom = true;
      } else {
        selectedPreset = "keyboard";
        showCustom = false;
      }
    } catch {}
    loading = false;
  }

  async function saveMapping() {
    if (configuringPort === null) return;
    const pad = configuringPort + 1;

    if (selectedPreset === "custom" && customDevice) {
      mapping.device = customDevice;
    } else if (selectedPreset === "keyboard") {
      mapping.device = "DInput/0/Keyboard Mouse";
    } else {
      mapping.device = selectedPreset;
    }

    try {
      await invoke("save_gcpad_mapping", { pad, mapping });
      saved = true;
      statusMsg = "Saved to Dolphin config!";
      setTimeout(() => { saved = false; statusMsg = ""; }, 2000);
    } catch (e: any) {
      alert("Failed to save: " + e.toString());
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<!-- ═══ GAMEPAD LISTEN OVERLAY (press a button) ═══ -->
{#if listening && isGamepad() && !showManualPicker}
  <div class="listen-overlay" role="dialog" onclick={stopListening}>
    <div class="listen-modal" onclick={(e) => e.stopPropagation()}>
      <div class="listen-pulse"></div>
      <p class="listen-title">Press a button or move a stick for</p>
      <p class="listen-button-name">{listening.replace(/_/g, " ").toUpperCase()}</p>
      <p class="listen-hint">Press any button on your controller... Click outside to cancel</p>
      <div class="manual-fallback">
        <button class="btn-manual" onclick={() => { showManualPicker = true; }}>
          Manual Pick
        </button>
      </div>
    </div>
  </div>
{/if}

<!-- ═══ MANUAL PICKER FALLBACK ═══ -->
{#if listening && showManualPicker}
  <div class="listen-overlay" role="dialog" onclick={() => { showManualPicker = false; stopListening(); }}>
    <div class="listen-modal" onclick={(e) => e.stopPropagation()}>
      <p class="listen-title">Pick input for</p>
      <p class="listen-button-name">{listening.replace(/_/g, " ").toUpperCase()}</p>
      <div class="input-picker">
        {#each DOLPHIN_INPUTS as group}
          <div class="picker-group">
            <span class="picker-group-title">{group.group}</span>
            <div class="picker-buttons">
              {#each group.options as opt}
                <button class="picker-btn" onclick={() => { showManualPicker = false; pickInput(opt.value); }}>
                  <span class="picker-btn-label">{opt.label}</span>
                  <span class="picker-btn-value">{opt.value}</span>
                </button>
              {/each}
            </div>
          </div>
        {/each}
      </div>
      <p class="listen-hint">Click outside to cancel</p>
    </div>
  </div>
{/if}

<!-- ═══ KEY LISTENER OVERLAY (keyboard mode) ═══ -->
{#if listening && !isGamepad() && !showManualPicker}
  <div class="listen-overlay" role="dialog" onclick={stopListening}>
    <div class="listen-modal">
      <div class="listen-pulse"></div>
      <p class="listen-title">Press a key for</p>
      <p class="listen-button-name">{listening.replace(/_/g, " ").toUpperCase()}</p>
      <p class="listen-hint">Press Escape to cancel</p>
    </div>
  </div>
{/if}

<!-- ═══ CONFIGURE OVERLAY (detailed mapping) ═══ -->
{#if configuringPort !== null}
  <div class="configure-overlay">
    <div class="configure-panel">
      <div class="configure-header">
        <h3 class="configure-title">GameCube Controller at Port {configuringPort + 1}</h3>
        <button class="btn-close" onclick={closeConfigure}>&times;</button>
      </div>

      <!-- Device selector -->
      <div class="device-section">
        <div class="device-row">
          <label class="field-label">Device</label>
          <select class="device-dropdown" bind:value={selectedPreset} onchange={onDeviceChange}>
            {#if dolphinDevices.length > 0}
              <optgroup label="Detected Devices">
                {#each dolphinDevices as dev}
                  <option value={dev === "DInput/0/Keyboard Mouse" ? "keyboard" : dev}>{dev}</option>
                {/each}
              </optgroup>
            {/if}
            {#each DEVICE_PRESETS as group}
              <optgroup label={group.group}>
                {#each group.options as opt}
                  {@const val = opt.value === "DInput/0/Keyboard Mouse" ? "keyboard" : opt.value}
                  {#if !dolphinDevices.includes(opt.value)}
                    <option value={val}>{opt.label}</option>
                  {/if}
                {/each}
              </optgroup>
            {/each}
            <optgroup label="Other">
              <option value="custom">Custom Device String...</option>
            </optgroup>
          </select>
        </div>

        {#if showCustom}
          <div class="custom-device">
            <input
              type="text"
              class="custom-input"
              bind:value={customDevice}
              placeholder="e.g. SDL/0/Xbox Wireless Controller"
              oninput={() => { mapping.device = customDevice; }}
            />
            <p class="custom-hint">Open Dolphin → Controllers → Port 1 to see your exact device string</p>
          </div>
        {/if}
      </div>

      {#if loading}
        <div class="loading">Loading...</div>
      {:else}
        <!-- Mapping sections - Dolphin style -->
        <div class="mapping-sections">
          <!-- Buttons -->
          <div class="map-section">
            <h4 class="section-title">Buttons</h4>
            <div class="map-grid">
              <div class="map-row">
                <span class="map-label">A</span>
                <button class="map-btn" onclick={() => startListening("a")}>{displayKey(mapping.a)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">B</span>
                <button class="map-btn" onclick={() => startListening("b")}>{displayKey(mapping.b)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">X</span>
                <button class="map-btn" onclick={() => startListening("x")}>{displayKey(mapping.x)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">Y</span>
                <button class="map-btn" onclick={() => startListening("y")}>{displayKey(mapping.y)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">Z</span>
                <button class="map-btn" onclick={() => startListening("z")}>{displayKey(mapping.z)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">Start</span>
                <button class="map-btn" onclick={() => startListening("start")}>{displayKey(mapping.start)}</button>
              </div>
            </div>
          </div>

          <!-- Main Stick -->
          <div class="map-section">
            <h4 class="section-title">Control Stick</h4>
            <div class="map-grid">
              <div class="map-row">
                <span class="map-label">Up</span>
                <button class="map-btn" onclick={() => startListening("stick_up")}>{displayKey(mapping.stick_up)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">Down</span>
                <button class="map-btn" onclick={() => startListening("stick_down")}>{displayKey(mapping.stick_down)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">Left</span>
                <button class="map-btn" onclick={() => startListening("stick_left")}>{displayKey(mapping.stick_left)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">Right</span>
                <button class="map-btn" onclick={() => startListening("stick_right")}>{displayKey(mapping.stick_right)}</button>
              </div>
            </div>
          </div>

          <!-- C-Stick -->
          <div class="map-section">
            <h4 class="section-title">C Stick</h4>
            <div class="map-grid">
              <div class="map-row">
                <span class="map-label">Up</span>
                <button class="map-btn" onclick={() => startListening("cstick_up")}>{displayKey(mapping.cstick_up)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">Down</span>
                <button class="map-btn" onclick={() => startListening("cstick_down")}>{displayKey(mapping.cstick_down)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">Left</span>
                <button class="map-btn" onclick={() => startListening("cstick_left")}>{displayKey(mapping.cstick_left)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">Right</span>
                <button class="map-btn" onclick={() => startListening("cstick_right")}>{displayKey(mapping.cstick_right)}</button>
              </div>
            </div>
          </div>

          <!-- Triggers -->
          <div class="map-section">
            <h4 class="section-title">Triggers</h4>
            <div class="map-grid">
              <div class="map-row">
                <span class="map-label">L</span>
                <button class="map-btn" onclick={() => startListening("l")}>{displayKey(mapping.l)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">R</span>
                <button class="map-btn" onclick={() => startListening("r")}>{displayKey(mapping.r)}</button>
              </div>
            </div>
          </div>

          <!-- D-Pad -->
          <div class="map-section">
            <h4 class="section-title">D-Pad</h4>
            <div class="map-grid">
              <div class="map-row">
                <span class="map-label">Up</span>
                <button class="map-btn" onclick={() => startListening("dpad_up")}>{displayKey(mapping.dpad_up)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">Down</span>
                <button class="map-btn" onclick={() => startListening("dpad_down")}>{displayKey(mapping.dpad_down)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">Left</span>
                <button class="map-btn" onclick={() => startListening("dpad_left")}>{displayKey(mapping.dpad_left)}</button>
              </div>
              <div class="map-row">
                <span class="map-label">Right</span>
                <button class="map-btn" onclick={() => startListening("dpad_right")}>{displayKey(mapping.dpad_right)}</button>
              </div>
            </div>
          </div>
        </div>

        <div class="configure-actions">
          {#if statusMsg}
            <span class="status-msg">{statusMsg}</span>
          {/if}
          <button class="btn-reset" onclick={onDeviceChange}>Default</button>
          <button class="btn-save" onclick={saveMapping}>{saved ? "Saved!" : "Save"}</button>
        </div>
      {/if}
    </div>
  </div>
{/if}

<!-- ═══ MAIN VIEW: Port List (Dolphin-style) ═══ -->
<div class="controller-config">
  <h3 class="group-title">GAMECUBE CONTROLLERS</h3>

  <div class="port-list">
    {#each [0, 1, 2, 3] as port}
      <div class="port-row">
        <span class="port-label">Port {port + 1}</span>
        <select class="port-dropdown" bind:value={portTypes[port]}>
          {#each PORT_TYPES as type}
            <option value={type}>{type}</option>
          {/each}
        </select>
        <button
          class="btn-configure"
          disabled={portTypes[port] === "None"}
          onclick={() => openConfigure(port)}
        >
          Configure
        </button>
      </div>
    {/each}
  </div>
</div>

<style>
  /* ── Main port list ── */
  .controller-config {
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

  .port-list {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .port-row {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 14px;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 8px;
  }

  .port-label {
    font-family: 'Orbitron', monospace;
    font-size: 12px;
    font-weight: 700;
    color: var(--text-primary);
    min-width: 56px;
  }

  .port-dropdown {
    flex: 1;
    padding: 8px 12px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-primary);
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
  }

  .port-dropdown:focus {
    border-color: var(--wind-cyan);
    outline: none;
  }

  .port-dropdown option {
    background: var(--bg-card);
    color: var(--text-primary);
  }

  .btn-configure {
    padding: 8px 18px;
    background: var(--bg-card-hover);
    color: var(--text-secondary);
    border: 1px solid var(--border);
    border-radius: 6px;
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.15s ease;
    white-space: nowrap;
  }

  .btn-configure:hover:not(:disabled) {
    color: var(--text-primary);
    border-color: var(--wind-cyan);
  }

  .btn-configure:disabled {
    opacity: 0.3;
    cursor: not-allowed;
  }

  /* ── Configure overlay ── */
  .configure-overlay {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.7);
    z-index: 5000;
    display: flex;
    align-items: center;
    justify-content: center;
    backdrop-filter: blur(4px);
  }

  .configure-panel {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 12px;
    width: 640px;
    max-height: 85vh;
    overflow-y: auto;
    padding: 24px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.5);
  }

  .configure-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 20px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--border);
  }

  .configure-title {
    font-family: 'Orbitron', monospace;
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 2px;
    color: var(--wind-cyan);
  }

  .btn-close {
    width: 32px;
    height: 32px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-muted);
    font-size: 18px;
    cursor: pointer;
    transition: all 0.15s ease;
  }

  .btn-close:hover {
    color: var(--text-primary);
    border-color: var(--text-muted);
  }

  /* ── Device section ── */
  .device-section {
    margin-bottom: 20px;
  }

  .device-row {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .field-label {
    font-family: 'Orbitron', monospace;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 1px;
    color: var(--text-muted);
    min-width: 52px;
  }

  .device-dropdown {
    flex: 1;
    padding: 8px 12px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-primary);
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
  }

  .device-dropdown:focus { border-color: var(--wind-cyan); outline: none; }
  .device-dropdown option { background: var(--bg-card); color: var(--text-primary); }
  .device-dropdown optgroup { color: var(--wind-cyan); font-weight: 700; font-size: 11px; }

  .custom-device { margin-top: 10px; }
  .custom-input {
    width: 100%;
    padding: 8px 12px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-primary);
    font-size: 13px;
    font-family: monospace;
    box-sizing: border-box;
  }
  .custom-input:focus { border-color: var(--wind-cyan); outline: none; }
  .custom-hint { font-size: 10px; color: var(--text-muted); margin-top: 4px; }

  /* ── Mapping sections ── */
  .mapping-sections {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 16px;
  }

  .map-section {
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 14px;
  }

  .section-title {
    font-family: 'Orbitron', monospace;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 2px;
    color: var(--wind-cyan);
    margin-bottom: 10px;
  }

  .map-grid {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .map-row {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .map-label {
    font-size: 12px;
    font-weight: 600;
    color: var(--text-secondary);
    min-width: 44px;
  }

  .map-btn {
    flex: 1;
    padding: 6px 10px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 4px;
    color: var(--accent-primary);
    font-size: 12px;
    font-weight: 700;
    font-family: 'Orbitron', monospace;
    cursor: pointer;
    text-align: left;
    transition: all 0.15s ease;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .map-btn:hover {
    border-color: var(--accent-primary);
    background: var(--bg-card-hover);
  }

  /* ── Configure actions ── */
  .configure-actions {
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: 10px;
    margin-top: 16px;
    padding-top: 12px;
    border-top: 1px solid var(--border);
  }

  .status-msg { font-size: 11px; color: var(--wind-cyan); font-family: monospace; margin-right: auto; }

  .btn-reset {
    padding: 8px 18px;
    background: var(--bg-primary);
    color: var(--text-secondary);
    border: 1px solid var(--border);
    font-size: 12px;
    font-weight: 600;
    border-radius: 6px;
    cursor: pointer;
    transition: all 0.15s ease;
  }
  .btn-reset:hover { color: var(--text-primary); border-color: var(--text-muted); }

  .btn-save {
    padding: 8px 24px;
    background: linear-gradient(135deg, var(--accent-primary), var(--wind-cyan));
    color: white;
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 1px;
    border-radius: 6px;
    border: none;
    cursor: pointer;
    transition: all 0.15s ease;
  }
  .btn-save:hover { box-shadow: 0 0 16px rgba(255, 107, 0, 0.3); transform: translateY(-1px); }

  .loading { text-align: center; color: var(--text-muted); padding: 40px; }

  /* ── Listen overlay ── */
  .listen-overlay { position: fixed; top: 0; left: 0; right: 0; bottom: 0; background: rgba(0, 0, 0, 0.9); z-index: 10000; display: flex; align-items: center; justify-content: center; backdrop-filter: blur(4px); cursor: default; border: none; }
  .listen-modal { text-align: center; display: flex; flex-direction: column; align-items: center; gap: 8px; max-height: 85vh; max-width: 90vw; }
  .listen-pulse { width: 60px; height: 60px; border: 3px solid var(--accent-primary); border-radius: 50%; animation: pulse 1.5s infinite; margin-bottom: 8px; }
  @keyframes pulse { 0% { transform: scale(1); opacity: 1; } 50% { transform: scale(1.3); opacity: 0.3; } 100% { transform: scale(1); opacity: 1; } }
  .listen-title { font-size: 14px; color: var(--text-secondary); }
  .listen-button-name { font-family: 'Orbitron', monospace; font-size: 20px; font-weight: 700; color: var(--accent-primary); letter-spacing: 2px; }
  .listen-hint { font-size: 11px; color: var(--text-muted); margin-top: 8px; }

  .manual-fallback { margin-top: 16px; }
  .btn-manual {
    padding: 6px 16px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-muted);
    font-size: 11px;
    cursor: pointer;
    transition: all 0.15s ease;
  }
  .btn-manual:hover { color: var(--text-primary); border-color: var(--wind-cyan); }

  .input-picker { display: flex; flex-direction: column; gap: 12px; margin-top: 12px; overflow-y: auto; max-height: 55vh; padding: 4px 8px; text-align: left; width: 560px; }
  .picker-group { display: flex; flex-direction: column; gap: 6px; }
  .picker-group-title { font-family: 'Orbitron', monospace; font-size: 9px; font-weight: 700; letter-spacing: 2px; color: var(--wind-cyan); padding-left: 4px; }
  .picker-buttons { display: grid; grid-template-columns: repeat(4, 1fr); gap: 4px; }
  .picker-btn { display: flex; flex-direction: column; align-items: center; gap: 2px; padding: 8px 6px; background: var(--bg-card); border: 1px solid var(--border); border-radius: 6px; cursor: pointer; transition: all 0.15s ease; }
  .picker-btn:hover { border-color: var(--accent-primary); background: var(--bg-card-hover); transform: translateY(-1px); }
  .picker-btn-label { font-size: 12px; font-weight: 700; color: var(--text-primary); }
  .picker-btn-value { font-size: 8px; color: var(--text-muted); font-family: monospace; }
</style>
