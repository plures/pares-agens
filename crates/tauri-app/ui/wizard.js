/**
 * Pares Agens — First-run wizard
 *
 * Flow:
 *   Step 0 — Welcome    (agent name)
 *   Step 1 — Model      (local / cloud / skip)
 *   Step 2 — Personality (system prompt, optional)
 *   Step 3 — Channel    (Telegram token, optional)
 *   Step 4 — Done       (summary + "Start chatting")
 *
 * State is persisted to localStorage so the user can close the window
 * mid-flow and resume where they left off.
 *
 * Completion is recorded in localStorage ("wizard_completed") so the
 * wizard never shows again after finishing.  The backend is also notified
 * via `complete_wizard` so that the runtime settings are applied.
 */

const { invoke } = window.__TAURI__.core;

// ── Storage keys ──────────────────────────────────────────────────────────────

const LS_COMPLETED  = "wizard_completed";
const LS_STATE      = "wizard_state";
const TOTAL_STEPS   = 5;

// ── Constants ─────────────────────────────────────────────────────────────────

/** Default model ID for Docker Model Runner.
 *  Docker Model Runner uses the "ai/<name>" image-pull format instead of
 *  the Ollama "<name>:<tag>" convention. */
const DOCKER_MODEL    = "ai/qwen3";
/** Docker Model Runner endpoint (port 12434 is its fixed default). */
const DOCKER_ENDPOINT = "http://localhost:12434/engines/llama.cpp/v1";

// ── DOM refs ──────────────────────────────────────────────────────────────────

const overlay       = document.getElementById("wizard-overlay");
const progressFill  = document.getElementById("wizard-progress-fill");
const agentHeading  = document.getElementById("agent-name-heading");
const welcomeName   = document.getElementById("welcome-agent-name");

// Step elements
const steps = Array.from({ length: TOTAL_STEPS }, (_, i) =>
  document.getElementById(`wizard-step-${i}`)
);

// ── Wizard state (persisted in localStorage) ──────────────────────────────────

/** @type {{ step: number, agentName: string, modelSource: string, cloudProvider: string, apiKey: string, systemPrompt: string, telegramToken: string }} */
let state = {
  step:           0,
  agentName:      "",
  modelSource:    "skip",
  cloudProvider:  "openai",
  apiKey:         "",
  systemPrompt:   "",
  telegramToken:  "",
};

function loadState() {
  try {
    const raw = localStorage.getItem(LS_STATE);
    if (raw) {
      const parsed = JSON.parse(raw);
      // Never load or keep a persisted Telegram token from localStorage.
      const hadTelegramToken = Object.prototype.hasOwnProperty.call(parsed, "telegramToken");
      if (hadTelegramToken) {
        delete parsed.telegramToken;
        // Overwrite stored state without the credential to clean up old data.
        localStorage.setItem(LS_STATE, JSON.stringify(parsed));
      }
      Object.assign(state, parsed);
    }
  } catch (_) { /* start fresh */ }
}

function saveState() {
  try {
    // Deliberately exclude apiKey and telegramToken from localStorage — the user
    // must re-enter these credentials if they reopen the wizard, preventing
    // sensitive values from being written to disk in plain text.
    const {
      apiKey: _omitApiKey,
      telegramToken: _omitTelegramToken,
      ...persistable
    } = state;
    localStorage.setItem(LS_STATE, JSON.stringify(persistable));
  } catch (_) { /* non-critical */ }
}

// ── Navigation helpers ────────────────────────────────────────────────────────

function showStep(n) {
  steps.forEach((s, i) => s.hidden = (i !== n));
  progressFill.style.width = `${((n + 1) / TOTAL_STEPS) * 100}%`;
  state.step = n;
  saveState();
  // Focus first interactive element in the new step
  const first = steps[n].querySelector("input, select, textarea, button");
  if (first) first.focus();
}

function goNext() { showStep(state.step + 1); }
function goBack() { showStep(state.step - 1); }

// ── Step 0 — Welcome ──────────────────────────────────────────────────────────

const nameInput   = document.getElementById("wizard-agent-name");
const chipRow     = document.querySelector(".wizard-chip-row");

function applyStoredName() {
  if (state.agentName) nameInput.value = state.agentName;
}

chipRow.addEventListener("click", (e) => {
  const chip = e.target.closest(".wizard-chip");
  if (chip) {
    nameInput.value = chip.dataset.value;
    nameInput.dispatchEvent(new Event("input"));
  }
});

nameInput.addEventListener("input", () => {
  state.agentName = nameInput.value.trim();
  saveState();
});

document.getElementById("wizard-next-0").addEventListener("click", () => {
  state.agentName = nameInput.value.trim() || "Pares Agens";
  saveState();
  goNext();
});

// ── Step 1 — Model ────────────────────────────────────────────────────────────

const modelRadios    = document.querySelectorAll('input[name="model-source"]');
const cloudSubForm   = document.getElementById("cloud-sub-form");
const providerSelect = document.getElementById("wizard-cloud-provider");
const apiKeyInput    = document.getElementById("wizard-api-key");
const apiKeyStatus   = document.getElementById("api-key-status");
const dockerStatus   = document.getElementById("docker-status");
const validateBtn    = document.getElementById("btn-validate-key");

function applyStoredModelSource() {
  const radio = document.querySelector(`input[name="model-source"][value="${state.modelSource}"]`);
  if (radio) radio.checked = true;
  providerSelect.value = state.cloudProvider || "openai";
  // Don't pre-fill apiKey for security — user must re-enter if they reopen
  cloudSubForm.hidden = (state.modelSource !== "cloud");
}

modelRadios.forEach((r) => {
  r.addEventListener("change", () => {
    state.modelSource = r.value;
    cloudSubForm.hidden = (r.value !== "cloud");
    if (r.value === "local") runDockerDetect();
    saveState();
  });
});

providerSelect.addEventListener("change", () => {
  state.cloudProvider = providerSelect.value;
  apiKeyStatus.textContent = "";
  saveState();
});

apiKeyInput.addEventListener("input", () => {
  state.apiKey = apiKeyInput.value;
  apiKeyStatus.textContent = "";
  // saveState() intentionally excludes apiKey — see the saveState() implementation
  saveState();
});

/** Probe Docker Model Runner and update the badge on the local card. */
async function runDockerDetect() {
  dockerStatus.textContent = "Checking…";
  dockerStatus.className = "wizard-badge badge-info";
  try {
    const found = await invoke("detect_docker_runner");
    if (found) {
      dockerStatus.textContent = "Running ✓";
      dockerStatus.className = "wizard-badge badge-ok";
    } else {
      dockerStatus.textContent = "Not found";
      dockerStatus.className = "wizard-badge badge-warn";
    }
  } catch (_) {
    dockerStatus.textContent = "Error";
    dockerStatus.className = "wizard-badge badge-warn";
  }
}

validateBtn.addEventListener("click", async () => {
  const key = apiKeyInput.value.trim();
  if (!key) {
    apiKeyStatus.textContent = "Please enter an API key.";
    apiKeyStatus.className = "wizard-hint hint-warn";
    return;
  }
  validateBtn.disabled = true;
  apiKeyStatus.textContent = "Validating…";
  apiKeyStatus.className = "wizard-hint";
  try {
    const valid = await invoke("validate_api_key", {
      provider: providerSelect.value,
      apiKey: key,
    });
    if (valid) {
      apiKeyStatus.textContent = "✓ Key accepted";
      apiKeyStatus.className = "wizard-hint hint-ok";
    } else {
      apiKeyStatus.textContent = "✗ Key rejected — check your key and try again";
      apiKeyStatus.className = "wizard-hint hint-error";
    }
  } catch (err) {
    apiKeyStatus.textContent = `Error: ${err}`;
    apiKeyStatus.className = "wizard-hint hint-error";
  } finally {
    validateBtn.disabled = false;
  }
});

document.getElementById("wizard-next-1").addEventListener("click", () => {
  if (state.modelSource === "cloud" && !state.apiKey) {
    // Warn but allow proceeding — cloud model will not work until an API key is configured
    apiKeyStatus.textContent = "⚠ No API key entered — cloud model will be unavailable until an API key is configured.";
    apiKeyStatus.className = "wizard-hint hint-warn";
  }
  saveState();
  goNext();
});
document.getElementById("wizard-back-1").addEventListener("click", goBack);

// ── Step 2 — Personality ──────────────────────────────────────────────────────

const systemPromptInput = document.getElementById("wizard-system-prompt");

function applyStoredSystemPrompt() {
  if (state.systemPrompt) systemPromptInput.value = state.systemPrompt;
}

systemPromptInput.addEventListener("input", () => {
  state.systemPrompt = systemPromptInput.value;
  saveState();
});

document.getElementById("wizard-next-2").addEventListener("click", () => {
  state.systemPrompt = systemPromptInput.value.trim();
  saveState();
  goNext();
});
document.getElementById("wizard-skip-2").addEventListener("click", () => {
  state.systemPrompt = "";
  saveState();
  goNext();
});
document.getElementById("wizard-back-2").addEventListener("click", goBack);

// ── Step 3 — Channel ──────────────────────────────────────────────────────────

const telegramInput = document.getElementById("wizard-telegram-token");

telegramInput.addEventListener("input", () => {
  state.telegramToken = telegramInput.value;
  saveState();
});

document.getElementById("wizard-next-3").addEventListener("click", () => {
  state.telegramToken = telegramInput.value.trim();
  saveState();
  buildSummary();
  goNext();
});
document.getElementById("wizard-skip-3").addEventListener("click", () => {
  state.telegramToken = "";
  saveState();
  buildSummary();
  goNext();
});
document.getElementById("wizard-back-3").addEventListener("click", goBack);

// ── Step 4 — Done ─────────────────────────────────────────────────────────────

function buildSummary() {
  const list    = document.getElementById("wizard-summary-list");
  const summary = document.getElementById("wizard-done-summary");
  const name    = state.agentName || "Pares Agens";

  summary.textContent = `${name} is ready.`;
  list.innerHTML = "";

  const items = [
    { label: "Agent name", value: name },
    {
      label: "Model",
      value:
        state.modelSource === "local" ? "Docker Model Runner (local)"
        : state.modelSource === "cloud" ? `Cloud — ${state.cloudProvider}`
        : "Not configured (configure later in Settings)",
    },
    {
      label: "Personality",
      value: state.systemPrompt ? "Custom system prompt set" : "Default",
    },
    {
      label: "Channel",
      value: state.telegramToken ? "Telegram connected" : "Desktop only",
    },
  ];

  for (const { label, value } of items) {
    const li = document.createElement("li");
    li.className = "wizard-summary-item";
    const b = document.createElement("span");
    b.className = "wizard-summary-label";
    b.textContent = `${label}: `;
    li.appendChild(b);
    li.appendChild(document.createTextNode(value));
    list.appendChild(li);
  }
}

document.getElementById("wizard-finish").addEventListener("click", async () => {
  await finishWizard();
});

/** Compile wizard choices into Settings and notify the backend. */
async function finishWizard() {
  const name = state.agentName || "Pares Agens";

  // Derive model + endpoint from the user's choice
  let model    = "qwen3:235b";
  let endpoint = "http://localhost:11434/v1";
  let apiKey   = null;

  if (state.modelSource === "local") {
    model    = DOCKER_MODEL;
    endpoint = DOCKER_ENDPOINT;
  } else if (state.modelSource === "cloud") {
    apiKey = state.apiKey || null;
    switch (state.cloudProvider) {
      case "openai":
        model    = "gpt-4o-mini";
        endpoint = "https://api.openai.com/v1";
        break;
      case "anthropic":
        model    = "claude-3-5-haiku-20241022";
        endpoint = "https://api.anthropic.com/v1";
        break;
      case "google":
        model    = "gemini-1.5-flash";
        endpoint = "https://generativelanguage.googleapis.com/v1beta";
        break;
    }
  }

  const systemPrompt = state.systemPrompt
    || `You are ${name}, a helpful desktop AI assistant.`;

  const channel = state.telegramToken ? "telegram" : "tauri";

  const settings = {
    model,
    endpoint,
    channel,
    systemPrompt,
    ...(apiKey ? { apiKey } : {}),
  };

  try {
    await invoke("complete_wizard", { settings });
  } catch (err) {
    console.error("complete_wizard failed, falling back to set_settings:", err);
    // Fallback: at minimum save settings separately
    try { await invoke("set_settings", { settings }); } catch (e) {
      console.error("set_settings fallback also failed:", e);
    }
  }

  // Persist completion durably in localStorage
  localStorage.setItem(LS_COMPLETED, "1");
  localStorage.removeItem(LS_STATE);

  // Update chat header with the chosen name
  if (agentHeading)  agentHeading.textContent  = name;
  if (welcomeName)   welcomeName.textContent    = name;

  // Hide the wizard
  overlay.hidden = true;
  overlay.setAttribute("aria-hidden", "true");

  // Focus the chat input
  const chatInput = document.getElementById("chat-input");
  if (chatInput) chatInput.focus();
}

// ── Entry point ───────────────────────────────────────────────────────────────

/** Show the wizard if it has not been completed. */
function initWizard() {
  // Fast local check (avoids IPC round-trip for returning users)
  if (localStorage.getItem(LS_COMPLETED)) return;

  loadState();
  applyStoredName();
  applyStoredModelSource();
  applyStoredSystemPrompt();

  // Normalize step from persisted state (localStorage) to a safe, in-range value
  const rawStep = typeof state.step === "number"
    ? state.step
    : parseInt(state.step, 10);
  let step = Number.isFinite(rawStep) ? rawStep : 0;
  if (step < 0) step = 0;
  if (step >= TOTAL_STEPS) step = TOTAL_STEPS - 1;
  state.step = step;

  overlay.hidden = false;
  overlay.removeAttribute("aria-hidden");
  showStep(step);

  // Probe Docker in background in case user arrives at step 1 quickly
  runDockerDetect();
}

initWizard();
