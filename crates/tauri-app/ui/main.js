/**
 * Pares Agens — Tauri frontend
 *
 * Architecture:
 *  - invoke("send_message", { content })  → ModelResponse content
 *  - invoke("get_settings")               → Settings object
 *  - invoke("set_settings", { settings }) → void
 *
 * The Tauri backend exposes these commands via src/commands.rs.
 * All IPC goes through the TauriIpcAdapter in the channels crate.
 */

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
const btnSave       = document.getElementById("btn-save-settings");
const btnCancel     = document.getElementById("btn-cancel-settings");
const agentNameHeading = document.getElementById("agent-name-heading");

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
  sender.textContent = role === "user"
    ? "You"
    : (agentNameHeading?.textContent || "Pares Agens");

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
    document.getElementById("s-auto-start").checked  = s.autoStart     ?? false;
  } catch (_) { /* proceed with whatever is in the inputs */ }
  settingsDialog.showModal();
}

async function saveSettings() {
  const settings = {
    model:        document.getElementById("s-model").value,
    endpoint:     document.getElementById("s-endpoint").value,
    systemPrompt: document.getElementById("s-system-prompt").value,
    channel:      document.getElementById("s-channel").value,
    autoStart:    document.getElementById("s-auto-start").checked,
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

// ── Init ──────────────────────────────────────────────────────────────────

// Open settings dialog when the tray "Settings" menu item is clicked.
// listen() returns a Promise resolving to an unlisten fn; since this
// listener must live for the entire app lifetime we fire-and-forget but
// surface any registration errors to the console.
listen("show-settings", () => openSettings()).catch(console.error);

refreshMemories();
chatInput.focus();

// ── Procedures ────────────────────────────────────────────────────────────

const btnProcedures    = document.getElementById("btn-procedures");
const proceduresDialog = document.getElementById("procedures-dialog");
const btnCloseProc     = document.getElementById("btn-close-procedures");
const procList         = document.getElementById("proc-list");
const procTemplateSelect = document.getElementById("proc-template-select");
const btnCreateProc    = document.getElementById("btn-create-proc");
const procEmpty        = document.getElementById("proc-empty");
const procEditorView   = document.getElementById("proc-editor-view");
const procDetailName   = document.getElementById("proc-detail-name");
const procDetailType   = document.getElementById("proc-detail-type");
const procEnabledToggle = document.getElementById("proc-enabled-toggle");
const btnEditProc      = document.getElementById("btn-edit-proc");
const btnSaveProc      = document.getElementById("btn-save-proc");
const procBody         = document.getElementById("proc-body");
const procLogBody      = document.getElementById("proc-log-body");

/** @type {string|null} — name of the currently selected procedure */
let selectedProcName = null;
/** @type {boolean} — whether the editor is in edit mode */
let editMode = false;

/** Render the procedure list from the given array of records. */
function renderProcList(records) {
  procList.innerHTML = "";
  if (!records || records.length === 0) {
    const li = document.createElement("li");
    li.className = "proc-list-empty";
    li.textContent = "No procedures registered.";
    li.style.cssText = "color:var(--text-muted);font-size:12px;padding:12px;text-align:center";
    procList.appendChild(li);
    return;
  }
  for (const rec of records) {
    const li = document.createElement("li");
    li.setAttribute("role", "option");
    li.setAttribute("aria-selected", rec.name === selectedProcName ? "true" : "false");
    li.dataset.name = rec.name;

    const dot = document.createElement("span");
    dot.className = `proc-status-dot ${rec.enabled ? "enabled" : "disabled"}`;
    dot.title = rec.enabled ? "Enabled" : "Disabled";

    const nameSpan = document.createElement("span");
    nameSpan.className = "proc-list-name";
    nameSpan.textContent = rec.name;

    const typeSpan = document.createElement("span");
    typeSpan.className = "proc-list-type";
    typeSpan.textContent = rec.eventType;

    li.appendChild(dot);
    li.appendChild(nameSpan);
    li.appendChild(typeSpan);

    li.addEventListener("click", () => selectProcedure(rec.name));
    procList.appendChild(li);
  }
}

/** Load and show the detail view for a named procedure. */
async function selectProcedure(name) {
  try {
    const rec = await invoke("get_procedure", { name });
    if (!rec) return;

    selectedProcName = name;

    // Update list selection highlight
    for (const li of procList.querySelectorAll("li")) {
      li.setAttribute("aria-selected", li.dataset.name === name ? "true" : "false");
    }

    procDetailName.textContent = rec.name;
    procDetailType.textContent = rec.eventType;
    procEnabledToggle.checked = rec.enabled;
    procBody.value = rec.body;

    // Reset to read-only view
    setEditMode(false);

    procEmpty.hidden = true;
    procEditorView.hidden = false;

    // Load execution log for this procedure
    await refreshProcLog(name);
  } catch (err) {
    console.error("selectProcedure error:", err);
  }
}

/** Switch between edit and read-only mode. */
function setEditMode(on) {
  editMode = on;
  procBody.readOnly = !on;
  btnEditProc.textContent = on ? "Cancel" : "Edit";
  btnEditProc.setAttribute("aria-pressed", on ? "true" : "false");
  btnSaveProc.hidden = !on;
}

/** Persist the current procedure body to the backend. */
async function saveProcedure() {
  if (!selectedProcName) return;
  try {
    const rec = await invoke("get_procedure", { name: selectedProcName });
    if (!rec) return;
    rec.body = procBody.value;
    await invoke("save_procedure", { record: rec });
    setEditMode(false);
    await refreshProcList();
  } catch (err) {
    alert(`Failed to save procedure: ${err}`);
  }
}

/** Reload the procedure list from the backend. */
async function refreshProcList() {
  try {
    const records = await invoke("list_procedures");
    renderProcList(records);
  } catch (_) {
    renderProcList([]);
  }
}

/** Reload the execution log for a procedure. */
async function refreshProcLog(name) {
  try {
    const entries = await invoke("get_procedure_log", { name, limit: 50 });
    procLogBody.innerHTML = "";
    if (!entries || entries.length === 0) {
      const tr = document.createElement("tr");
      tr.innerHTML = `<td colspan="3" class="log-empty">No executions recorded yet.</td>`;
      procLogBody.appendChild(tr);
      return;
    }
    for (const e of entries) {
      const tr = document.createElement("tr");
      const time = document.createElement("td");
      time.textContent = new Date(e.firedAt).toLocaleTimeString();
      const dur = document.createElement("td");
      dur.textContent = `${e.durationMs} ms`;
      const trigger = document.createElement("td");
      trigger.textContent = e.triggerEvent;
      tr.appendChild(time);
      tr.appendChild(dur);
      tr.appendChild(trigger);
      procLogBody.appendChild(tr);
    }
  } catch (_) { /* log is non-critical */ }
}

// ── Enable/disable toggle ──────────────────────────────────────────────────

procEnabledToggle.addEventListener("change", async () => {
  if (!selectedProcName) return;
  try {
    await invoke("toggle_procedure", {
      name: selectedProcName,
      enabled: procEnabledToggle.checked,
    });
    await refreshProcList();
  } catch (err) {
    // Revert the toggle on failure
    procEnabledToggle.checked = !procEnabledToggle.checked;
    alert(`Failed to toggle procedure: ${err}`);
  }
});

// ── Edit / Save buttons ────────────────────────────────────────────────────

btnEditProc.addEventListener("click", () => setEditMode(!editMode));
btnSaveProc.addEventListener("click", saveProcedure);

// ── Create from template ───────────────────────────────────────────────────

btnCreateProc.addEventListener("click", async () => {
  const template = procTemplateSelect.value;
  if (!template) {
    alert("Please select a template first.");
    return;
  }
  try {
    const rec = await invoke("create_from_template", { template });
    procTemplateSelect.value = "";
    await refreshProcList();
    await selectProcedure(rec.name);
  } catch (err) {
    alert(`Failed to create procedure: ${err}`);
  }
});

// ── Open / close dialog ────────────────────────────────────────────────────

async function openProcedures() {
  selectedProcName = null;
  editMode = false;
  procEmpty.hidden = false;
  procEditorView.hidden = true;
  await refreshProcList();
  proceduresDialog.showModal();
}

btnProcedures.addEventListener("click", openProcedures);
btnCloseProc.addEventListener("click", () => proceduresDialog.close());

// Close on backdrop click
proceduresDialog.addEventListener("click", (e) => {
  if (e.target === proceduresDialog) proceduresDialog.close();
});
