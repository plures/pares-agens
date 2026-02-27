/**
 * Pares Agens — Tauri frontend
 *
 * Architecture:
 *  - invoke("send_message", { content })        → ModelResponse content
 *  - invoke("get_settings")                     → Settings object
 *  - invoke("set_settings", { settings })       → void
 *  - invoke("get_license_status")               → LicenseStatus object
 *  - invoke("activate_license", { key })        → LicenseStatus object
 *
 * The Tauri backend exposes these commands via src/commands.rs.
 * All IPC goes through the TauriIpcAdapter in the channels crate.
 */

const { invoke } = window.__TAURI__.core;

// ── DOM refs ──────────────────────────────────────────────────────────────

const messageList   = document.getElementById("message-list");
const chatForm      = document.getElementById("chat-form");
const chatInput     = document.getElementById("chat-input");
const sendBtn       = document.getElementById("send-btn");
const statusDot     = document.getElementById("agent-status");
const memoryList    = document.getElementById("memory-list");
const btnSettings   = document.getElementById("btn-settings");
const settingsDialog = document.getElementById("settings-dialog");
const btnSave       = document.getElementById("btn-save-settings");
const btnCancel     = document.getElementById("btn-cancel-settings");

// ── Utilities ─────────────────────────────────────────────────────────────

/** Format a Date as HH:MM */
function fmtTime(date) {
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** Auto-grow textarea up to max-height */
function autoGrow(el) {
  el.style.height = "auto";
  el.style.height = Math.min(el.scrollHeight, 140) + "px";
}

/** Escape HTML to prevent XSS when rendering raw text as innerHTML */
function escHtml(str) {
  return str
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** Scroll message list to the bottom */
function scrollToBottom() {
  messageList.scrollTop = messageList.scrollHeight;
}

// ── Message rendering ─────────────────────────────────────────────────────

/**
 * Append a message bubble to the conversation.
 * @param {"user"|"agent"} role
 * @param {string} content
 * @returns {HTMLElement} the appended bubble element
 */
function appendMessage(role, content) {
  // Remove the welcome placeholder on first real message
  const welcome = messageList.querySelector(".welcome-message");
  if (welcome) welcome.remove();

  const wrapper = document.createElement("div");
  wrapper.className = `message ${role}`;

  const sender = document.createElement("span");
  sender.className = "message-sender";
  sender.textContent = role === "user" ? "You" : "Pares Agens";

  const bubble = document.createElement("div");
  bubble.className = "message-bubble";
  bubble.textContent = content;          // textContent — safe, no HTML injection

  const time = document.createElement("span");
  time.className = "message-time";
  time.textContent = fmtTime(new Date());

  wrapper.appendChild(sender);
  wrapper.appendChild(bubble);
  wrapper.appendChild(time);
  messageList.appendChild(wrapper);
  scrollToBottom();
  return bubble;
}

/** Show an animated typing indicator while the agent is thinking. */
function showTypingIndicator() {
  const wrapper = document.createElement("div");
  wrapper.className = "message agent typing-indicator";
  wrapper.id = "typing-indicator";

  const sender = document.createElement("span");
  sender.className = "message-sender";
  sender.textContent = "Pares Agens";

  const bubble = document.createElement("div");
  bubble.className = "message-bubble";
  for (let i = 0; i < 3; i++) {
    const dot = document.createElement("span");
    dot.className = "typing-dot";
    bubble.appendChild(dot);
  }

  wrapper.appendChild(sender);
  wrapper.appendChild(bubble);
  messageList.appendChild(wrapper);
  scrollToBottom();
}

function hideTypingIndicator() {
  const el = document.getElementById("typing-indicator");
  if (el) el.remove();
}

// ── Memory sidebar ────────────────────────────────────────────────────────

const CATEGORY_CSS = {
  "code-pattern": "memory-code",
  preference:     "memory-pref",
  decision:       "memory-dec",
  "error-fix":    "memory-err",
};

/**
 * Fetch recent memories from the Rust backend and re-render the sidebar.
 * No-ops silently if the backend returns an error (memories are non-critical).
 */
async function refreshMemories() {
  try {
    const memories = await invoke("get_memories");
    memoryList.innerHTML = "";

    if (!memories || memories.length === 0) {
      const empty = document.createElement("li");
      empty.className = "memory-empty";
      empty.textContent = "No memories yet.";
      memoryList.appendChild(empty);
      return;
    }

    for (const m of memories) {
      const li = document.createElement("li");
      const cssClass = CATEGORY_CSS[m.category] || "";
      if (cssClass) li.classList.add(cssClass);

      const cat = document.createElement("span");
      cat.className = "memory-category";
      cat.textContent = m.category;

      const content = document.createElement("span");
      content.className = "memory-content";
      content.textContent = m.content;

      li.appendChild(cat);
      li.appendChild(document.createElement("br"));
      li.appendChild(content);
      li.title = m.content;
      memoryList.appendChild(li);
    }
  } catch (_err) {
    // Memories are non-critical — swallow the error silently.
  }
}

// ── Send message ──────────────────────────────────────────────────────────

/** True while a request is in-flight; prevents double-submits. */
let isBusy = false;

async function sendMessage(content) {
  if (isBusy || !content.trim()) return;

  isBusy = true;
  sendBtn.disabled = true;
  statusDot.className = "status-dot thinking";

  appendMessage("user", content);
  showTypingIndicator();

  try {
    const response = await invoke("send_message", { content: content.trim() });
    hideTypingIndicator();
    if (response) appendMessage("agent", response);
    // Refresh memory sidebar after each exchange.
    refreshMemories();
  } catch (err) {
    hideTypingIndicator();
    appendMessage("agent", `⚠ Error: ${err}`);
  } finally {
    isBusy = false;
    sendBtn.disabled = false;
    statusDot.className = "status-dot online";
  }
}

// ── Event handlers ────────────────────────────────────────────────────────

chatForm.addEventListener("submit", (e) => {
  e.preventDefault();
  const content = chatInput.value;
  chatInput.value = "";
  autoGrow(chatInput);
  sendMessage(content);
});

chatInput.addEventListener("input", () => autoGrow(chatInput));

chatInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    chatForm.dispatchEvent(new Event("submit"));
  }
});

// ── Settings ──────────────────────────────────────────────────────────────

async function openSettings() {
  try {
    const s = await invoke("get_settings");
    document.getElementById("s-model").value         = s.model         ?? "";
    document.getElementById("s-endpoint").value      = s.endpoint      ?? "";
    document.getElementById("s-system-prompt").value = s.systemPrompt  ?? "";
    document.getElementById("s-channel").value       = s.channel       ?? "tauri";
  } catch (_) { /* proceed with whatever is in the inputs */ }
  await refreshLicenseStatus();
  settingsDialog.showModal();
}

async function saveSettings() {
  const settings = {
    model:        document.getElementById("s-model").value,
    endpoint:     document.getElementById("s-endpoint").value,
    systemPrompt: document.getElementById("s-system-prompt").value,
    channel:      document.getElementById("s-channel").value,
  };
  try {
    await invoke("set_settings", { settings });
    settingsDialog.close();
  } catch (err) {
    alert(`Failed to save settings: ${err}`);
  }
}

btnSettings.addEventListener("click", openSettings);
btnSave.addEventListener("click", saveSettings);
btnCancel.addEventListener("click", () => settingsDialog.close());

// Close dialog on backdrop click
settingsDialog.addEventListener("click", (e) => {
  if (e.target === settingsDialog) settingsDialog.close();
});

// ── License / Upgrade ────────────────────────────────────────────────────

const licenseBadge    = document.getElementById("license-badge");
const btnUpgrade      = document.getElementById("btn-upgrade");
const upgradeDialog   = document.getElementById("upgrade-dialog");
const btnCloseUpgrade = document.getElementById("btn-close-upgrade");
const btnCancelUpgrade = document.getElementById("btn-cancel-upgrade");
const btnActivate     = document.getElementById("btn-activate");
const licenseKeyInput = document.getElementById("license-key-input");
const upgradeError    = document.getElementById("upgrade-error");

/**
 * Refresh the license badge in the Settings > License fieldset.
 * Called on open-settings and after activation.
 */
async function refreshLicenseStatus() {
  try {
    const status = await invoke("get_license_status");
    const isPro  = status.tier === "pro" && status.valid;
    licenseBadge.textContent = isPro ? "Pro" : "Free";
    licenseBadge.className   = `license-badge ${isPro ? "license-pro" : "license-free"}`;
    btnUpgrade.hidden         = isPro;
  } catch (_) {
    // Non-critical; leave badge in its current state.
  }
}

/** Show the upgrade/activation dialog. */
function openUpgradeDialog() {
  licenseKeyInput.value  = "";
  upgradeError.hidden    = true;
  upgradeError.textContent = "";
  upgradeDialog.showModal();
  licenseKeyInput.focus();
}

btnUpgrade.addEventListener("click", openUpgradeDialog);
btnCloseUpgrade.addEventListener("click", () => upgradeDialog.close());
btnCancelUpgrade.addEventListener("click", () => upgradeDialog.close());

// Close on backdrop click.
upgradeDialog.addEventListener("click", (e) => {
  if (e.target === upgradeDialog) upgradeDialog.close();
});

/** Attempt to activate the entered license key. */
async function activateLicense() {
  const key = licenseKeyInput.value.trim();
  if (!key) {
    upgradeError.textContent = "Please enter a license key.";
    upgradeError.hidden = false;
    return;
  }

  btnActivate.disabled = true;
  upgradeError.hidden  = true;

  try {
    const status = await invoke("activate_license", { key });
    upgradeDialog.close();
    await refreshLicenseStatus();

    if (status.tier === "pro" && status.valid) {
      appendMessage("agent", "🎉 Pro license activated! All Pro features are now unlocked.");
    }
  } catch (err) {
    upgradeError.textContent = `Activation failed: ${err}`;
    upgradeError.hidden = false;
  } finally {
    btnActivate.disabled = false;
  }
}

btnActivate.addEventListener("click", activateLicense);

// Allow Enter key in the key input to trigger activation.
licenseKeyInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") activateLicense();
});

// ── Init ──────────────────────────────────────────────────────────────────

refreshMemories();
chatInput.focus();
