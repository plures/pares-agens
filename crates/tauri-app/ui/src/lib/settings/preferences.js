/**
 * preferences.js — General agent / UX preferences UI.
 *
 * Exports `renderPreferences(container, prefs, settings)` and
 * `readPreferences(container)`.
 */

const DEFAULT_CATEGORIES = ["code-pattern", "preference", "decision", "error-fix"];

/**
 * Render the Preferences tab panel.
 *
 * @param {HTMLElement} container  Tab-panel container.
 * @param {object}      prefs      `AgentPreferences` from backend.
 * @param {object}      settings   Full `Settings` object (for system_prompt).
 */
export function renderPreferences(container, prefs, settings) {
  container.innerHTML = "";

  // ── Agent identity ────────────────────────────────────────────────────
  _section(container, "Agent Identity", [
    _textField("pref-agent-name", "Agent Name", prefs.agentName ?? "Pares Agens",
      "Pares Agens"),
    _textareaField("pref-system-prompt", "System Prompt",
      settings.systemPrompt ?? "You are Pares Agens, a helpful desktop AI assistant.",
      3),
    _textareaField("pref-personality", "Personality Notes",
      prefs.personalityNotes ?? "",
      2,
      "Additional personality traits or instructions appended to the system prompt"),
  ]);

  // ── Memory behaviour ──────────────────────────────────────────────────
  _section(container, "Memory Behaviour", [
    _toggleRow("pref-auto-recall", "Auto-recall",
      "Retrieve relevant memories automatically on each turn",
      prefs.autoRecall ?? true),
    _checkboxGroup("pref-capture-cat", "Capture Categories",
      DEFAULT_CATEGORIES,
      prefs.captureCategories ?? ["code-pattern", "preference", "decision"]),
  ]);

  // ── Notifications ─────────────────────────────────────────────────────
  _section(container, "Notifications", [
    _toggleRow("pref-notifications", "Desktop notifications",
      "Show a system notification when the agent responds",
      prefs.notificationsEnabled ?? true),
  ]);
}

/**
 * Read back all preference values from the rendered form.
 *
 * @param {HTMLElement} container
 * @returns {{ prefs: object, systemPrompt: string }}
 */
export function readPreferences(container) {
  const prefs = {
    agentName:           _val("pref-agent-name"),
    personalityNotes:    _val("pref-personality"),
    autoRecall:          _checked("pref-auto-recall"),
    captureCategories:   Array.from(
      container.querySelectorAll(".capture-cat-check:checked"),
    ).map((el) => el.value),
    notificationsEnabled: _checked("pref-notifications"),
  };
  const systemPrompt = _val("pref-system-prompt");
  return { prefs, systemPrompt };
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

function _section(container, title, rows) {
  const sec = document.createElement("section");
  sec.className = "pref-section";

  const hdr = document.createElement("h3");
  hdr.className = "pref-section-title";
  hdr.textContent = title;
  sec.appendChild(hdr);

  for (const row of rows) sec.appendChild(row);
  container.appendChild(sec);
}

function _textField(id, label, value, placeholder = "") {
  const lbl = document.createElement("label");
  lbl.textContent = label;
  const inp = document.createElement("input");
  inp.type = "text";
  inp.id = id;
  inp.value = value;
  inp.placeholder = placeholder;
  lbl.appendChild(inp);
  return lbl;
}

function _textareaField(id, label, value, rows = 3, placeholder = "") {
  const lbl = document.createElement("label");
  lbl.textContent = label;
  const ta = document.createElement("textarea");
  ta.id = id;
  ta.rows = rows;
  ta.value = value;
  ta.placeholder = placeholder;
  lbl.appendChild(ta);
  return lbl;
}

function _toggleRow(id, label, hint, checked) {
  const row = document.createElement("div");
  row.className = "pref-toggle-row";

  const textEl = document.createElement("div");
  textEl.className = "pref-toggle-text";

  const lblEl = document.createElement("span");
  lblEl.className = "pref-label";
  lblEl.textContent = label;

  const hintEl = document.createElement("span");
  hintEl.className = "pref-hint";
  hintEl.textContent = hint;

  textEl.appendChild(lblEl);
  textEl.appendChild(hintEl);

  const toggle = document.createElement("label");
  toggle.className = "toggle";
  toggle.setAttribute("aria-label", label);

  const cb = document.createElement("input");
  cb.type = "checkbox";
  cb.id = id;
  cb.className = "toggle-input";
  cb.checked = checked;

  const slider = document.createElement("span");
  slider.className = "toggle-slider";
  slider.setAttribute("aria-hidden", "true");

  toggle.appendChild(cb);
  toggle.appendChild(slider);

  row.appendChild(textEl);
  row.appendChild(toggle);
  return row;
}

function _checkboxGroup(groupId, label, allOptions, selected) {
  const wrapper = document.createElement("div");
  wrapper.className = "pref-checkbox-group";
  wrapper.id = groupId;

  const lbl = document.createElement("span");
  lbl.className = "pref-label";
  lbl.textContent = label;
  wrapper.appendChild(lbl);

  const grid = document.createElement("div");
  grid.className = "checkbox-grid";

  for (const opt of allOptions) {
    const item = document.createElement("label");
    item.className = "checkbox-item";

    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.className = "capture-cat-check";
    cb.value = opt;
    cb.checked = selected.includes(opt);

    item.appendChild(cb);
    item.append(` ${opt}`);
    grid.appendChild(item);
  }

  wrapper.appendChild(grid);
  return wrapper;
}

function _val(id) {
  return document.getElementById(id)?.value ?? "";
}

function _checked(id) {
  return document.getElementById(id)?.checked ?? false;
}
