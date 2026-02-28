/**
 * providers.js — Model provider CRUD UI.
 *
 * Exports a single `renderProviders(container, providers, invoke, onRefresh)`
 * function that builds the Providers tab panel content.
 */

/**
 * Render the providers list plus an "Add Provider" button into `container`.
 *
 * @param {HTMLElement}  container  The tab-panel container element.
 * @param {object[]}     providers  Array of masked provider objects from the backend.
 * @param {Function}     invoke     Tauri `invoke` function.
 * @param {Function}     onRefresh  Callback to re-render after a mutation.
 */
export function renderProviders(container, providers, invoke, onRefresh) {
  container.innerHTML = "";

  if (providers.length === 0) {
    const empty = document.createElement("p");
    empty.className = "settings-empty";
    empty.textContent = "No providers configured.";
    container.appendChild(empty);
  } else {
    container.appendChild(_buildTable(providers, invoke, onRefresh));
  }

  const addBtn = document.createElement("button");
  addBtn.type = "button";
  addBtn.className = "btn-secondary";
  addBtn.textContent = "+ Add Provider";
  addBtn.addEventListener("click", () =>
    _showForm(container, null, invoke, onRefresh),
  );
  container.appendChild(addBtn);
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

function _buildTable(providers, invoke, onRefresh) {
  const table = document.createElement("table");
  table.className = "provider-table";

  const thead = table.createTHead();
  const hrow = thead.insertRow();
  for (const h of ["Name", "Endpoint", "Models", "API Key", ""]) {
    const th = document.createElement("th");
    th.textContent = h;
    hrow.appendChild(th);
  }

  const tbody = table.createTBody();
  for (const p of providers) {
    const row = tbody.insertRow();
    row.insertCell().textContent = p.name;
    row.insertCell().textContent = p.baseUrl;
    row.insertCell().textContent = (p.models ?? []).join(", ") || "—";
    row.insertCell().textContent = p.apiKey ? "••••••••" : "—";

    const actCell = row.insertCell();
    actCell.className = "provider-actions";

    const editBtn = document.createElement("button");
    editBtn.type = "button";
    editBtn.className = "icon-btn";
    editBtn.title = "Edit";
    editBtn.setAttribute("aria-label", `Edit provider ${p.name}`);
    editBtn.textContent = "✎";
    editBtn.addEventListener("click", () => {
      const panel = table.closest(".tab-panel");
      _showForm(panel, p, invoke, onRefresh);
    });

    const removeBtn = document.createElement("button");
    removeBtn.type = "button";
    removeBtn.className = "icon-btn danger";
    removeBtn.title = "Remove";
    removeBtn.setAttribute("aria-label", `Remove provider ${p.name}`);
    removeBtn.textContent = "✕";
    removeBtn.addEventListener("click", async () => {
      if (!confirm(`Remove provider "${p.name}"?`)) return;
      try {
        await invoke("remove_provider", { name: p.name });
        onRefresh();
      } catch (err) {
        alert(`Failed to remove provider: ${err}`);
      }
    });

    actCell.appendChild(editBtn);
    actCell.appendChild(removeBtn);
  }

  return table;
}

function _showForm(container, existing, invoke, onRefresh) {
  // Remove any pre-existing inline form.
  container.querySelector(".provider-form")?.remove();

  const form = document.createElement("div");
  form.className = "provider-form settings-inline-form";

  const title = document.createElement("h3");
  title.textContent = existing ? `Edit "${existing.name}"` : "Add Provider";
  form.appendChild(title);

  const fieldDefs = [
    {
      id: "pf-name",
      label: "Name",
      type: "text",
      value: existing?.name ?? "",
      placeholder: "ollama",
      disabled: !!existing,
    },
    {
      id: "pf-url",
      label: "Endpoint URL",
      type: "url",
      value: existing?.baseUrl ?? "",
      placeholder: "http://localhost:11434/v1",
    },
    {
      id: "pf-key",
      label: "API Key",
      type: "password",
      value: existing?.apiKey ?? "",
      placeholder: "sk-… (leave blank for none)",
    },
    {
      id: "pf-models",
      label: "Models (comma-separated)",
      type: "text",
      value: (existing?.models ?? []).join(", "),
      placeholder: "qwen3:235b, llama3.1:8b",
    },
  ];

  for (const f of fieldDefs) {
    const lbl = document.createElement("label");
    lbl.textContent = f.label;
    const inp = document.createElement("input");
    inp.type = f.type;
    inp.id = f.id;
    inp.value = f.value;
    inp.placeholder = f.placeholder;
    if (f.disabled) inp.disabled = true;
    lbl.appendChild(inp);
    form.appendChild(lbl);
  }

  const actions = document.createElement("div");
  actions.className = "inline-form-actions";

  const cancelBtn = document.createElement("button");
  cancelBtn.type = "button";
  cancelBtn.className = "btn-secondary";
  cancelBtn.textContent = "Cancel";
  cancelBtn.addEventListener("click", () => form.remove());

  const saveBtn = document.createElement("button");
  saveBtn.type = "button";
  saveBtn.className = "btn-primary";
  saveBtn.textContent = existing ? "Update" : "Add";
  saveBtn.addEventListener("click", async () => {
    const name    = document.getElementById("pf-name").value.trim();
    const baseUrl = document.getElementById("pf-url").value.trim();
    const rawKey  = document.getElementById("pf-key").value;
    const apiKey  = rawKey.trim() || null;
    const models  = document
      .getElementById("pf-models")
      .value.split(",")
      .map((m) => m.trim())
      .filter(Boolean);

    if (!name || !baseUrl) {
      alert("Name and Endpoint URL are required.");
      return;
    }

    // Always include apiKey so the backend can clear it when set to null.
    const payload = { name, baseUrl, models, apiKey };

    try {
      if (existing) {
        await invoke("update_provider", { name: existing.name, provider: payload });
      } else {
        await invoke("add_provider", { provider: payload });
      }
      form.remove();
      onRefresh();
    } catch (err) {
      alert(`Failed to save provider: ${err}`);
    }
  });

  actions.appendChild(cancelBtn);
  actions.appendChild(saveBtn);
  form.appendChild(actions);
  container.appendChild(form);
  document.getElementById("pf-name")?.focus();
}
