/**
 * routing.js — Per-use-case model routing preferences UI.
 *
 * Exports `renderRouting(container, routing, providers)` which builds a
 * simple form for selecting a provider + model for each of the three use-case
 * slots: interactive, background, and coding.
 */

const USE_CASES = [
  { key: "interactive", label: "Interactive", hint: "Real-time chat responses" },
  { key: "background",  label: "Background",  hint: "Long-running or async tasks" },
  { key: "coding",      label: "Coding",       hint: "Code generation and editing" },
];

/**
 * Render the routing preferences form into `container`.
 *
 * The form is read back in `main.js` via `readRouting(container)` when the
 * user hits Save.
 *
 * @param {HTMLElement} container  Tab-panel container.
 * @param {object}      routing    Current `RoutingPrefs` from backend.
 * @param {object[]}    providers  Provider list (masked) for populating selects.
 */
export function renderRouting(container, routing, providers) {
  container.innerHTML = "";

  if (providers.length === 0) {
    const note = document.createElement("p");
    note.className = "settings-empty";
    note.textContent = "Add at least one model provider before configuring routing.";
    container.appendChild(note);
    return;
  }

  const desc = document.createElement("p");
  desc.className = "settings-description";
  desc.textContent =
    "Choose which provider and model to use for each use case. " +
    "Leave a slot unset to use the global default.";
  container.appendChild(desc);

  for (const { key, label, hint } of USE_CASES) {
    const current = routing[key];
    const group = document.createElement("div");
    group.className = "routing-group";

    const legend = document.createElement("span");
    legend.className = "routing-label";
    legend.textContent = label;

    const hintEl = document.createElement("span");
    hintEl.className = "routing-hint";
    hintEl.textContent = hint;

    // Provider select
    const providerSel = document.createElement("select");
    providerSel.id = `rt-${key}-provider`;
    providerSel.setAttribute("aria-label", `${label} provider`);

    const noneOpt = document.createElement("option");
    noneOpt.value = "";
    noneOpt.textContent = "— unset —";
    providerSel.appendChild(noneOpt);

    for (const p of providers) {
      const opt = document.createElement("option");
      opt.value = p.name;
      opt.textContent = p.name;
      if (current?.provider === p.name) opt.selected = true;
      providerSel.appendChild(opt);
    }

    // Model select — populated from the chosen provider's model list.
    const modelSel = document.createElement("select");
    modelSel.id = `rt-${key}-model`;
    modelSel.setAttribute("aria-label", `${label} model`);

    function populateModels(providerName) {
      modelSel.innerHTML = "";
      const none = document.createElement("option");
      none.value = "";
      none.textContent = "— unset —";
      modelSel.appendChild(none);

      const provider = providers.find((x) => x.name === providerName);
      for (const model of provider?.models ?? []) {
        const opt = document.createElement("option");
        opt.value = model;
        opt.textContent = model;
        if (current?.model === model && current?.provider === providerName) {
          opt.selected = true;
        }
        modelSel.appendChild(opt);
      }
    }

    populateModels(current?.provider ?? "");
    providerSel.addEventListener("change", () =>
      populateModels(providerSel.value),
    );

    group.appendChild(legend);
    group.appendChild(hintEl);
    group.appendChild(providerSel);
    group.appendChild(modelSel);
    container.appendChild(group);
  }
}

/**
 * Read the current routing form values from `container`.
 *
 * @param {HTMLElement} container
 * @returns {object} `RoutingPrefs`-shaped object.
 */
export function readRouting(container) {
  const result = {};
  for (const { key } of USE_CASES) {
    const provider = container.querySelector(`#rt-${key}-provider`)?.value ?? "";
    const model    = container.querySelector(`#rt-${key}-model`)?.value    ?? "";
    if (provider && model) {
      result[key] = { provider, model };
    }
  }
  return result;
}
