/**
 * channels.js — Channel adapter management UI.
 *
 * Exports `renderChannels(container, channelAdapters)` which builds enable /
 * disable toggles with per-adapter configuration fields.
 */

const ADAPTER_DEFS = [
  {
    kind: "local",
    label: "Local (desktop)",
    icon: "🖥",
    fields: [],
  },
  {
    kind: "telegram",
    label: "Telegram",
    icon: "✈",
    fields: [
      { id: "bot_token",    label: "Bot Token",    type: "password", placeholder: "123456:ABC-..." },
    ],
  },
  {
    kind: "signal",
    label: "Signal",
    icon: "🔒",
    fields: [
      { id: "phone_number", label: "Phone Number", type: "tel",      placeholder: "+1 555 000 0000" },
    ],
  },
];

/**
 * Render channel adapter cards into `container`.
 *
 * The form values are read back via `readChannelAdapters(container)` on Save.
 *
 * @param {HTMLElement} container       Tab-panel container.
 * @param {object[]}    channelAdapters Current adapter configs from backend.
 */
export function renderChannels(container, channelAdapters) {
  container.innerHTML = "";

  for (const def of ADAPTER_DEFS) {
    const existing = channelAdapters.find((a) => a.kind === def.kind);

    const card = document.createElement("div");
    card.className = "channel-card";
    card.dataset.kind = def.kind;

    // ── Header row ────────────────────────────────────────────────────────
    const header = document.createElement("div");
    header.className = "channel-header";

    const nameEl = document.createElement("span");
    nameEl.className = "channel-name";
    nameEl.textContent = `${def.icon}  ${def.label}`;

    const toggle = document.createElement("label");
    toggle.className = "toggle";
    toggle.setAttribute("aria-label", `Enable ${def.label}`);

    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.className = "toggle-input";
    checkbox.id = `ch-${def.kind}-enabled`;
    checkbox.checked = existing?.enabled ?? false;

    const slider = document.createElement("span");
    slider.className = "toggle-slider";
    slider.setAttribute("aria-hidden", "true");

    toggle.appendChild(checkbox);
    toggle.appendChild(slider);

    header.appendChild(nameEl);
    header.appendChild(toggle);
    card.appendChild(header);

    // ── Config fields ─────────────────────────────────────────────────────
    if (def.fields.length > 0) {
      const fieldsEl = document.createElement("div");
      fieldsEl.className = "channel-fields";

      for (const f of def.fields) {
        const lbl = document.createElement("label");
        lbl.textContent = f.label;

        const inp = document.createElement("input");
        inp.type = f.type;
        inp.id = `ch-${def.kind}-${f.id}`;
        inp.placeholder = f.placeholder;
        inp.value = existing?.[_camel(f.id)] ?? "";

        lbl.appendChild(inp);
        fieldsEl.appendChild(lbl);
      }

      // Show/hide config fields based on toggle state.
      fieldsEl.hidden = !checkbox.checked;
      checkbox.addEventListener("change", () => {
        fieldsEl.hidden = !checkbox.checked;
      });

      card.appendChild(fieldsEl);
    }

    container.appendChild(card);
  }
}

/**
 * Read all channel adapter configs from the rendered form.
 *
 * @param {HTMLElement} container
 * @returns {object[]} Array of `ChannelAdapterConfig`-shaped objects.
 */
export function readChannelAdapters(container) {
  const adapters = [];
  for (const def of ADAPTER_DEFS) {
    const enabled = container.querySelector(`#ch-${def.kind}-enabled`)?.checked ?? false;
    const entry = { kind: def.kind, enabled };

    for (const f of def.fields) {
      // Always include the field so users can explicitly clear a stored value.
      const input = container.querySelector(`#ch-${def.kind}-${f.id}`);
      const rawValue = input ? input.value : null;
      const trimmed = typeof rawValue === "string" ? rawValue.trim() : null;
      entry[_camel(f.id)] = trimmed === "" || trimmed === null ? null : trimmed;
    }

    adapters.push(entry);
  }
  return adapters;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Convert snake_case field id to camelCase for JS object keys. */
function _camel(s) {
  return s.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
}
