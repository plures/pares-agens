<script>
  const { invoke } = window.__TAURI__.core;

  let { open = $bindable(false) } = $props();

  /** @type {HTMLDialogElement} */
  let dialog = $state(null);

  // ── Tab state ───────────────────────────────────────────────────────────
  /** @type {'providers'|'routing'|'channels'|'preferences'} */
  let activeTab = $state('providers');
  /** @type {HTMLButtonElement[]} */
  let tabButtons = $state([]);

  const TABS = /** @type {const} */ (['providers', 'routing', 'channels', 'preferences']);

  // ── Provider state ───────────────────────────────────────────────────────
  /**
   * @typedef {{ name: string, baseUrl: string, apiKey: string|null, models: string[] }} Provider
   */
  /** @type {Provider[]} */
  let providers = $state([]);
  let showProviderForm = $state(false);
  let providerFormName = $state('');
  let providerFormUrl = $state('');
  let providerFormKey = $state('');
  let providerFormModels = $state('');
  /** @type {string|null} editProviderName is set when editing an existing provider */
  let editProviderName = $state(null);

  // ── Routing state ────────────────────────────────────────────────────────
  /**
   * @typedef {{ provider: string, model: string }} ModelRef
   * @typedef {{ interactive?: ModelRef, background?: ModelRef, coding?: ModelRef }} RoutingPrefs
   */
  let routingInteractiveProvider = $state('');
  let routingInteractiveModel = $state('');
  let routingBackgroundProvider = $state('');
  let routingBackgroundModel = $state('');
  let routingCodingProvider = $state('');
  let routingCodingModel = $state('');

  // ── Channel state ────────────────────────────────────────────────────────
  /**
   * @typedef {{ kind: string, enabled: boolean, botToken?: string, phoneNumber?: string }} ChannelAdapter
   */
  /** @type {ChannelAdapter[]} */
  let channelAdapters = $state([]);

  // ── Preferences state ────────────────────────────────────────────────────
  let prefAgentName = $state('');
  let prefPersonalityNotes = $state('');
  let prefAutoRecall = $state(true);
  let prefCaptureCategories = $state(/** @type {string[]} */ ([]));
  let prefNotificationsEnabled = $state(true);
  let prefAutoStart = $state(false);
  let prefSystemPrompt = $state('');

  const ALL_CAPTURE_CATEGORIES = ['code-pattern', 'preference', 'decision', 'error'];

  // ── Dialog lifecycle ─────────────────────────────────────────────────────
  $effect(() => {
    if (!dialog) return;
    if (open) {
      activeTab = 'providers';
      loadAll().then(() => dialog.showModal()).catch(() => dialog.showModal());
    } else {
      dialog.close();
    }
  });

  async function loadAll() {
    let s;
    try {
      s = await invoke('get_settings');
    } catch {
      open = false;
      return;
    }

    try {
      providers = await invoke('list_providers');
    } catch {
      providers = [];
    }

    // Routing
    const r = s.routing ?? {};
    routingInteractiveProvider = r.interactive?.provider ?? '';
    routingInteractiveModel    = r.interactive?.model    ?? '';
    routingBackgroundProvider  = r.background?.provider  ?? '';
    routingBackgroundModel     = r.background?.model     ?? '';
    routingCodingProvider      = r.coding?.provider      ?? '';
    routingCodingModel         = r.coding?.model         ?? '';

    // Channel adapters
    channelAdapters = (s.channelAdapters ?? []).map(a => ({ ...a }));

    // Preferences
    const p = s.preferences ?? {};
    prefAgentName              = p.agentName            ?? 'Pares Agens';
    prefPersonalityNotes       = p.personalityNotes     ?? '';
    prefAutoRecall             = p.autoRecall            ?? true;
    prefCaptureCategories      = p.captureCategories     ?? [];
    prefNotificationsEnabled   = p.notificationsEnabled  ?? true;
    prefAutoStart              = s.autoStart             ?? false;
    prefSystemPrompt           = s.systemPrompt          ?? '';
  }

  // ── Tab keyboard navigation (roving tabindex) ───────────────────────────
  function handleTabKeydown(/** @type {KeyboardEvent} */ e, idx) {
    let next = idx;
    if (e.key === 'ArrowRight' || e.key === 'ArrowDown') {
      next = (idx + 1) % TABS.length;
    } else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') {
      next = (idx - 1 + TABS.length) % TABS.length;
    } else if (e.key === 'Home') {
      next = 0;
    } else if (e.key === 'End') {
      next = TABS.length - 1;
    } else if (e.key === 'Enter' || e.key === ' ') {
      activeTab = TABS[idx];
      return;
    } else {
      return;
    }
    e.preventDefault();
    activeTab = TABS[next];
    tabButtons[next]?.focus();
  }

  // ── Provider CRUD ────────────────────────────────────────────────────────
  function openAddProvider() {
    editProviderName = null;
    providerFormName = '';
    providerFormUrl  = '';
    providerFormKey  = '';
    providerFormModels = '';
    showProviderForm = true;
  }

  function openEditProvider(/** @type {Provider} */ p) {
    editProviderName   = p.name;
    providerFormName   = p.name;
    providerFormUrl    = p.baseUrl;
    providerFormKey    = '';  // leave blank — backend preserves key when empty
    providerFormModels = (p.models ?? []).join(', ');
    showProviderForm   = true;
  }

  async function saveProvider() {
    const entry = {
      name:    providerFormName.trim(),
      baseUrl: providerFormUrl.trim(),
      apiKey:  providerFormKey.trim() || null,
      models:  providerFormModels.split(',').map(m => m.trim()).filter(Boolean),
    };
    try {
      if (editProviderName === null) {
        await invoke('add_provider', { provider: entry });
      } else {
        await invoke('update_provider', { name: editProviderName, provider: entry });
      }
      providers = await invoke('list_providers');
      showProviderForm = false;
    } catch (err) {
      alert(`Failed to save provider: ${err}`);
    }
  }

  async function deleteProvider(/** @type {string} */ name) {
    if (!confirm(`Remove provider "${name}"?`)) return;
    try {
      await invoke('remove_provider', { name });
      providers = await invoke('list_providers');
    } catch (err) {
      alert(`Failed to remove provider: ${err}`);
    }
  }

  // ── Channel adapter toggle ───────────────────────────────────────────────
  function getAdapter(/** @type {string} */ kind) {
    return channelAdapters.find(a => a.kind === kind);
  }

  function toggleAdapter(/** @type {string} */ kind) {
    const idx = channelAdapters.findIndex(a => a.kind === kind);
    if (idx >= 0) {
      channelAdapters[idx] = { ...channelAdapters[idx], enabled: !channelAdapters[idx].enabled };
    }
  }

  function setAdapterField(/** @type {string} */ kind, /** @type {string} */ field, /** @type {string} */ value) {
    const idx = channelAdapters.findIndex(a => a.kind === kind);
    if (idx >= 0) {
      channelAdapters[idx] = { ...channelAdapters[idx], [field]: value || null };
    }
  }

  // ── Capture category toggle ──────────────────────────────────────────────
  function toggleCategory(/** @type {string} */ cat) {
    if (prefCaptureCategories.includes(cat)) {
      prefCaptureCategories = prefCaptureCategories.filter(c => c !== cat);
    } else {
      prefCaptureCategories = [...prefCaptureCategories, cat];
    }
  }

  // ── Save all ─────────────────────────────────────────────────────────────
  async function saveAll() {
    try {
      // Reload fresh settings to carry over provider list (mutated via separate
      // CRUD commands) and any other fields the UI doesn't manage.
      const fresh = await invoke('get_settings');

      // Build routing object from UI state.
      const routing = {};
      if (routingInteractiveProvider && routingInteractiveModel) {
        routing.interactive = { provider: routingInteractiveProvider, model: routingInteractiveModel };
      }
      if (routingBackgroundProvider && routingBackgroundModel) {
        routing.background = { provider: routingBackgroundProvider, model: routingBackgroundModel };
      }
      if (routingCodingProvider && routingCodingModel) {
        routing.coding = { provider: routingCodingProvider, model: routingCodingModel };
      }

      // Single set_settings call: routing, channel_adapters, preferences, and
      // startup/system-prompt are all written atomically.  Provider CRUD was
      // already applied to the backend state; `fresh` carries those changes.
      await invoke('set_settings', {
        settings: {
          ...fresh,
          autoStart:       prefAutoStart,
          systemPrompt:    prefSystemPrompt,
          routing,
          channelAdapters: channelAdapters,
          preferences: {
            agentName:            prefAgentName,
            personalityNotes:     prefPersonalityNotes,
            autoRecall:           prefAutoRecall,
            captureCategories:    prefCaptureCategories,
            notificationsEnabled: prefNotificationsEnabled,
          },
        },
      });

      open = false;
    } catch (err) {
      alert(`Failed to save settings: ${err}`);
    }
  }

  function handleBackdropClick(e) {
    if (e.target === dialog) open = false;
  }
</script>

<dialog
  bind:this={dialog}
  class="settings-dialog"
  aria-label="Settings"
  onclick={handleBackdropClick}>

  <form method="dialog" onsubmit={(e) => e.preventDefault()}>
    <header class="dialog-header">
      <h2>Settings</h2>
      <button class="icon-btn close-btn" type="button"
        onclick={() => { open = false; }} aria-label="Close settings">✕</button>
    </header>

    <!-- Tab bar -->
    <div class="settings-tabs" role="tablist" aria-label="Settings sections">
      {#each TABS as tab, i}
        <button
          bind:this={tabButtons[i]}
          role="tab"
          type="button"
          id="tab-{tab}"
          aria-controls="panel-{tab}"
          aria-selected={activeTab === tab}
          tabindex={activeTab === tab ? 0 : -1}
          class="settings-tab"
          onclick={() => { activeTab = tab; }}
          onkeydown={(e) => handleTabKeydown(e, i)}>
          {tab.charAt(0).toUpperCase() + tab.slice(1)}
        </button>
      {/each}
    </div>

    <!-- Providers panel -->
    <div
      role="tabpanel"
      id="panel-providers"
      aria-labelledby="tab-providers"
      class="settings-panel"
      hidden={activeTab !== 'providers'}>

      {#if showProviderForm}
        <div class="provider-form">
          <h3 class="pref-section-title">{editProviderName === null ? 'Add Provider' : 'Edit Provider'}</h3>
          <label>
            Name
            <input type="text" bind:value={providerFormName}
              placeholder="ollama" readonly={editProviderName !== null} />
          </label>
          <label>
            Base URL
            <input type="url" bind:value={providerFormUrl}
              placeholder="http://localhost:11434/v1" />
          </label>
          <label>
            API Key <span class="pref-hint">{editProviderName !== null ? '(leave blank to keep existing)' : '(leave blank for local models)'}</span>
            <input type="password" bind:value={providerFormKey}
              placeholder={editProviderName !== null ? 'unchanged' : 'sk-…'} autocomplete="off" />
          </label>
          <label>
            Models <span class="pref-hint">(comma-separated)</span>
            <input type="text" bind:value={providerFormModels}
              placeholder="qwen3:235b, llama3.1:8b" />
          </label>
          <div class="provider-form-actions">
            <button type="button" class="btn-secondary"
              onclick={() => { showProviderForm = false; }}>Cancel</button>
            <button type="button" class="btn-primary-sm"
              onclick={saveProvider}>Save</button>
          </div>
        </div>
      {:else}
        <div class="panel-toolbar">
          <button type="button" class="btn-primary-sm"
            onclick={openAddProvider}>+ Add Provider</button>
        </div>
        {#if providers.length === 0}
          <p class="panel-empty">No providers configured.</p>
        {:else}
          <table class="provider-table">
            <thead>
              <tr>
                <th>Name</th>
                <th>Base URL</th>
                <th>Key</th>
                <th>Models</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {#each providers as p (p.name)}
                <tr>
                  <td class="provider-name">{p.name}</td>
                  <td class="provider-url">{p.baseUrl}</td>
                  <td class="provider-key">{p.apiKey ? '••••••••' : '—'}</td>
                  <td class="provider-models">{(p.models ?? []).join(', ') || '—'}</td>
                  <td class="provider-actions">
                    <button type="button" class="btn-icon-sm"
                      aria-label="Edit {p.name}"
                      onclick={() => openEditProvider(p)}>✎</button>
                    <button type="button" class="btn-icon-sm btn-danger"
                      aria-label="Remove {p.name}"
                      onclick={() => deleteProvider(p.name)}>✕</button>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      {/if}
    </div>

    <!-- Routing panel -->
    <div
      role="tabpanel"
      id="panel-routing"
      aria-labelledby="tab-routing"
      class="settings-panel"
      hidden={activeTab !== 'routing'}>

      <div class="pref-section">
        <p class="pref-section-title">Route each use-case to a specific provider and model.</p>

        {#each [
          { label: 'Interactive', providerVal: routingInteractiveProvider, modelVal: routingInteractiveModel,
            setProvider: v => { routingInteractiveProvider = v; }, setModel: v => { routingInteractiveModel = v; } },
          { label: 'Background', providerVal: routingBackgroundProvider, modelVal: routingBackgroundModel,
            setProvider: v => { routingBackgroundProvider = v; }, setModel: v => { routingBackgroundModel = v; } },
          { label: 'Coding', providerVal: routingCodingProvider, modelVal: routingCodingModel,
            setProvider: v => { routingCodingProvider = v; }, setModel: v => { routingCodingModel = v; } },
        ] as row}
          <div class="routing-row">
            <span class="routing-label">{row.label}</span>
            <select
              aria-label="{row.label} provider"
              value={row.providerVal}
              onchange={(e) => row.setProvider(e.currentTarget.value)}>
              <option value="">— provider —</option>
              {#each providers as p}
                <option value={p.name}>{p.name}</option>
              {/each}
            </select>
            <input type="text"
              aria-label="{row.label} model"
              placeholder="model ID"
              value={row.modelVal}
              oninput={(e) => row.setModel(e.currentTarget.value)} />
          </div>
        {/each}
      </div>
    </div>

    <!-- Channels panel -->
    <div
      role="tabpanel"
      id="panel-channels"
      aria-labelledby="tab-channels"
      class="settings-panel"
      hidden={activeTab !== 'channels'}>

      <div class="channel-cards">
        {#each channelAdapters as adapter (adapter.kind)}
          {@const enabled = adapter.enabled}
          <div class="channel-card" class:channel-card-active={enabled}>
            <div class="channel-card-header">
              <span class="channel-name">{adapter.kind}</span>
              <label class="toggle" aria-label="Enable {adapter.kind} channel">
                <input
                  class="toggle-input"
                  type="checkbox"
                  checked={enabled}
                  onchange={() => toggleAdapter(adapter.kind)} />
                <span class="toggle-slider" aria-hidden="true"></span>
              </label>
            </div>

            {#if enabled}
              <div class="channel-fields">
                {#if adapter.kind === 'telegram'}
                  <label>
                    Bot Token
                    <input type="password"
                      placeholder="123456:ABC-DEF…"
                      value={adapter.botToken ?? ''}
                      oninput={(e) => setAdapterField(adapter.kind, 'botToken', e.currentTarget.value)}
                      autocomplete="off" />
                  </label>
                {/if}
                {#if adapter.kind === 'signal'}
                  <label>
                    Phone Number
                    <input type="tel"
                      placeholder="+1 555 000 0000"
                      value={adapter.phoneNumber ?? ''}
                      oninput={(e) => setAdapterField(adapter.kind, 'phoneNumber', e.currentTarget.value)} />
                  </label>
                {/if}
              </div>
            {/if}
          </div>
        {/each}
      </div>
    </div>

    <!-- Preferences panel -->
    <div
      role="tabpanel"
      id="panel-preferences"
      aria-labelledby="tab-preferences"
      class="settings-panel"
      hidden={activeTab !== 'preferences'}>

      <div class="pref-section">
        <p class="pref-section-title">Identity</p>
        <label>
          Agent Name
          <input type="text" bind:value={prefAgentName} placeholder="Pares Agens" />
        </label>
        <label>
          Personality Notes
          <textarea bind:value={prefPersonalityNotes} rows="3"
            placeholder="Optional notes appended to the system prompt…"></textarea>
        </label>
        <label>
          System Prompt
          <textarea bind:value={prefSystemPrompt} rows="3"></textarea>
        </label>
      </div>

      <div class="pref-section">
        <p class="pref-section-title">Memory</p>
        <div class="pref-toggle-row">
          <div class="pref-toggle-text">
            <span class="pref-label">Auto-recall</span>
            <span class="pref-hint">Retrieve relevant memories each turn</span>
          </div>
          <label class="toggle" aria-label="Enable auto-recall">
            <input class="toggle-input" type="checkbox" bind:checked={prefAutoRecall} />
            <span class="toggle-slider" aria-hidden="true"></span>
          </label>
        </div>
        <div class="pref-checkbox-group">
          <span class="pref-hint">Capture categories</span>
          <div class="checkbox-grid">
            {#each ALL_CAPTURE_CATEGORIES as cat}
              <label class="checkbox-item">
                <input type="checkbox"
                  checked={prefCaptureCategories.includes(cat)}
                  onchange={() => toggleCategory(cat)} />
                {cat}
              </label>
            {/each}
          </div>
        </div>
      </div>

      <div class="pref-section">
        <p class="pref-section-title">Notifications &amp; Startup</p>
        <div class="pref-toggle-row">
          <div class="pref-toggle-text">
            <span class="pref-label">Desktop notifications</span>
            <span class="pref-hint">Alert when the agent responds</span>
          </div>
          <label class="toggle" aria-label="Enable desktop notifications">
            <input class="toggle-input" type="checkbox" bind:checked={prefNotificationsEnabled} />
            <span class="toggle-slider" aria-hidden="true"></span>
          </label>
        </div>
        <div class="pref-toggle-row">
          <div class="pref-toggle-text">
            <span class="pref-label">Launch at login</span>
            <span class="pref-hint">Start minimised to the system tray</span>
          </div>
          <label class="toggle" aria-label="Launch at login">
            <input class="toggle-input" type="checkbox" bind:checked={prefAutoStart} />
            <span class="toggle-slider" aria-hidden="true"></span>
          </label>
        </div>
      </div>
    </div>

    <footer class="dialog-footer">
      <button type="button" onclick={() => { open = false; }}>Cancel</button>
      <button type="button" class="btn-primary" onclick={saveAll}>Save</button>
    </footer>
  </form>
</dialog>
