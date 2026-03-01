/**
 * Pares Agens — Tauri frontend
 *
 * Architecture:
 *  - invoke("send_message",          { content })          → ModelResponse content
 *  - invoke("get_settings")                                → Settings object
 *  - invoke("set_settings",          { settings })         → void
 *  - invoke("list_providers")                              → ProviderEntry[] (keys masked)
 *  - invoke("add_provider",          { provider })         → void
 *  - invoke("update_provider",       { name, provider })   → void
 *  - invoke("remove_provider",       { name })             → void
 *  - invoke("upsert_channel_adapter",{ adapter })          → void
 *  - invoke("set_routing",           { routing })          → void
 *
 * The Tauri backend exposes these commands via src/commands.rs and
 * src/settings.rs.  All IPC goes through the TauriIpcAdapter in the channels
 * crate.
 */

import { renderProviders }                    from "./src/lib/settings/providers.js";
import { renderRouting, readRouting }         from "./src/lib/settings/routing.js";
import { renderChannels, readChannelAdapters } from "./src/lib/settings/channels.js";
import { renderPreferences, readPreferences } from "./src/lib/settings/preferences.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ── DOM refs ──────────────────────────────────────────────────────────────

const messageList   = document.getElementById("message-list");
const chatForm      = document.getElementById("chat-form");
const chatInput     = document.getElementById("chat-input");
const sendBtn       = document.getElementById("send-btn");
const statusDot     = document.getElementById("agent-status");
const memoryList    = document.getElementById("memory-list");
const btnSettings   = document.getElementById("btn-settings");
const settingsDialog = document.getElementById("settings-dialog");

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

/** Currently loaded settings — refreshed each time the dialog opens. */
let _currentSettings = null;

// Tab switching ──────────────────────────────────────────────────────────

const tabBtns   = settingsDialog.querySelectorAll(".tab-btn");
const tabPanels = settingsDialog.querySelectorAll(".tab-panel");
const tabBtnArray = Array.from(tabBtns);

function activateTab(targetId) {
  let activeBtn = null;

  for (const btn of tabBtns) {
    const controls = btn.getAttribute("aria-controls");
    const active = controls === targetId;
    btn.classList.toggle("active", active);
    btn.setAttribute("aria-selected", String(active));
    btn.setAttribute("tabindex", active ? "0" : "-1");
    if (active) {
      activeBtn = btn;
    }
  }

  for (const panel of tabPanels) {
    panel.hidden = panel.id !== targetId;
  }

  if (activeBtn) {
    activeBtn.focus();
  }
}

for (const btn of tabBtns) {
  btn.addEventListener("click", () =>
    activateTab(btn.getAttribute("aria-controls")),
  );

  btn.addEventListener("keydown", (event) => {
    const { key } = event;
    const currentIndex = tabBtnArray.indexOf(btn);
    if (currentIndex === -1) {
      return;
    }

    let newIndex = null;

    if (key === "ArrowRight") {
      newIndex = (currentIndex + 1) % tabBtnArray.length;
    } else if (key === "ArrowLeft") {
      newIndex = (currentIndex - 1 + tabBtnArray.length) % tabBtnArray.length;
    } else if (key === "Home") {
      newIndex = 0;
    } else if (key === "End") {
      newIndex = tabBtnArray.length - 1;
    } else if (key === "Enter" || key === " ") {
      // Activate currently focused tab
      const targetId = btn.getAttribute("aria-controls");
      if (targetId) {
        event.preventDefault();
        activateTab(targetId);
      }
      return;
    }

    if (newIndex !== null) {
      const nextBtn = tabBtnArray[newIndex];
      const targetId = nextBtn && nextBtn.getAttribute("aria-controls");
      if (targetId) {
        event.preventDefault();
        activateTab(targetId);
      }
    }
  });
}

// Open / close ───────────────────────────────────────────────────────────

async function openSettings() {
  try {
    _currentSettings = await invoke("get_settings");
  } catch (err) {
    console.error("Failed to load settings from backend:", err);
    // Abort opening the settings dialog to avoid saving invalid settings.
    return;
  }

  const providers = await _loadProviders();
  _renderAllTabs(providers);

  // Always start on the first tab.
  activateTab("tab-panel-providers");
  settingsDialog.showModal();
}

function closeSettings() {
  settingsDialog.close();
}

async function _loadProviders() {
  try {
    return await invoke("list_providers");
  } catch (_) {
    return [];
  }
}

function _renderAllTabs(providers) {
  renderProviders(
    document.getElementById("providers-content"),
    providers,
    invoke,
    async () => {
      const refreshed = await _loadProviders();
      _renderAllTabs(refreshed);
    },
  );

  renderRouting(
    document.getElementById("routing-content"),
    _currentSettings?.routing ?? {},
    providers,
  );

  renderChannels(
    document.getElementById("channels-content"),
    _currentSettings?.channelAdapters ?? [],
  );

  renderPreferences(
    document.getElementById("preferences-content"),
    _currentSettings?.preferences ?? {},
    _currentSettings ?? {},
  );
}

// Save ───────────────────────────────────────────────────────────────────

async function saveSettings() {
  const routingData  = readRouting(document.getElementById("routing-content"));
  const channelData  = readChannelAdapters(document.getElementById("channels-content"));
  const { prefs, systemPrompt } = readPreferences(
    document.getElementById("preferences-content"),
  );

  // Persist all settings (including routing and channel adapters) in a single call
  // to avoid partial updates if a later step fails.
  const updated = {
    ...(_currentSettings ?? {}),
    systemPrompt,
    preferences: prefs,
    // Keep routing and channelAdapters in sync on the full settings object.
    routing: routingData,
    channelAdapters: channelData,
  };

  try {
    await invoke("set_settings", { settings: updated });
    closeSettings();
  } catch (err) {
    alert(`Failed to save settings: ${err}`);
  }
}

// Wire up buttons ────────────────────────────────────────────────────────

btnSettings.addEventListener("click", openSettings);
document.getElementById("btn-save-settings").addEventListener("click", saveSettings);
document.getElementById("btn-cancel-settings").addEventListener("click", closeSettings);
document.getElementById("btn-close-settings").addEventListener("click", closeSettings);

// Close dialog on backdrop click.
settingsDialog.addEventListener("click", (e) => {
  if (e.target === settingsDialog) closeSettings();
});

// ── Init ──────────────────────────────────────────────────────────────────

// Open settings dialog when the tray "Settings" menu item is clicked.
// listen() returns a Promise resolving to an unlisten fn; since this
// listener must live for the entire app lifetime we fire-and-forget but
// surface any registration errors to the console.
listen("show-settings", () => openSettings()).catch(console.error);

refreshMemories();
chatInput.focus();
