<script>
  const { invoke } = window.__TAURI__.core;

  import MarketplaceTab from './MarketplaceTab.svelte';

  let { open = $bindable(false) } = $props();

  /** @type {HTMLDialogElement} */
  let dialog = $state(null);

  // ── Tab state ───────────────────────────────────────────────────────────
  /** @type {'providers'|'routing'|'channels'|'preferences'|'mcp'|'telemetry'|'license'|'marketplace'} */
  let activeTab = $state('providers');
  /** @type {HTMLButtonElement[]} */
  let tabButtons = $state([]);

  const TABS = /** @type {const} */ (['providers', 'routing', 'channels', 'preferences', 'mcp', 'telemetry', 'license', 'marketplace']);

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

  // ── Ollama quick-config state ────────────────────────────────────────────
  /** Ollama server base URL (without /v1 — the HTTP client appends that). */
  let ollamaUrl   = $state('http://localhost:11434');
  /** Model name to use for interactive chat. */
  let ollamaModel = $state('llama3');

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
  let prefActivationHotkey = $state('Ctrl+Space');
  let prefSystemPrompt = $state('');

  const ALL_CAPTURE_CATEGORIES = ['code-pattern', 'preference', 'decision', 'error'];

  // ── MCP state ────────────────────────────────────────────────────────────
  /**
   * @typedef {{ name: string, command: string, args: string[], enabled: boolean }} McpServer
   * @typedef {{ serverName: string, name: string, description: string|null, inputSchema: any }} DiscoveredTool
   */
  /** @type {McpServer[]} */
  let mcpServers = $state([]);
  /** @type {DiscoveredTool[]} */
  let mcpTools = $state([]);
  let showMcpForm = $state(false);
  let mcpFormName = $state('');
  let mcpFormCommand = $state('');
  let mcpFormArgs = $state('');
  let mcpFormEnabled = $state(true);
  /** @type {string|null} */
  let editMcpName = $state(null);
  let mcpRestarting = $state(false);

  // ── License state ────────────────────────────────────────────────────────
  /**
   * @typedef {{ tier: 'free'|'pro', valid: boolean, expires_at?: string }} LicenseStatus
   */
  /** @type {LicenseStatus} */
  let licenseStatus = $state({ tier: 'free', valid: true });
  let licenseKey = $state('');
  let licenseError = $state('');
  let licenseActivating = $state(false);

  // ── Telemetry state ──────────────────────────────────────────────────────
  let telemetryEnabled = $state(false);
  let telemetryUploadEnabled = $state(false);
  let telemetryUploadEndpoint = $state('');
  /** @type {{ modelCallsByDay?: Record<string, number>, toolUsageFrequency?: Record<string, number>, avgLatencyMs?: number|null, latencySampleCount?: number, latencyMinMs?: number|null, latencyMaxMs?: number|null, lastUploadAt?: string|null }} */
  let telemetrySnapshot = $state({});
  let telemetryUploading = $state(false);
  let telemetryUploadError = $state('');

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

    // Ollama quick config — read from the "ollama" provider entry when
    // present, otherwise fall back to the legacy endpoint/model fields.
    const ollamaProv = providers.find(p => p.name === 'ollama');
    ollamaUrl   = ollamaProv?.baseUrl ?? s.endpoint ?? 'http://localhost:11434';
    ollamaModel = s.routing?.interactive?.model ?? s.model ?? 'llama3';

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
    prefActivationHotkey       = s.activationHotkey      ?? 'Ctrl+Space';
    prefSystemPrompt           = s.systemPrompt          ?? '';

    // MCP servers
    mcpServers = (s.mcpServers ?? []).map(m => ({ ...m }));
    try {
      mcpTools = await invoke('list_mcp_tools');
    } catch {
      mcpTools = [];
    }

    // License
    try {
      licenseStatus = await invoke('get_license_status');
    } catch {
      licenseStatus = { tier: 'free', valid: true };
    }

    // Telemetry
    const t = s.telemetry ?? {};
    telemetryEnabled = t.enabled ?? false;
    telemetryUploadEnabled = t.uploadEnabled ?? false;
    telemetryUploadEndpoint = t.uploadEndpoint ?? '';
    try {
      telemetrySnapshot = await invoke('get_telemetry_snapshot');
    } catch {
      telemetrySnapshot = {};
    }
    telemetryUploadError = '';
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

      // ── Persist Ollama quick-config ────────────────────────────────────
      // Update (or add) the "ollama" provider entry with the URL the user
      // entered, so the model router picks up the change immediately.
      const ollamaBaseUrl   = ollamaUrl.trim()   || 'http://localhost:11434';
      const ollamaModelVal  = ollamaModel.trim()  || 'llama3';
      const existingOllama  = providers.find(p => p.name === 'ollama');
      try {
        if (existingOllama) {
          await invoke('update_provider', {
            name: 'ollama',
            provider: {
              name:    'ollama',
              baseUrl: ollamaBaseUrl,
              apiKey:  null,
              models:  existingOllama.models ?? [ollamaModelVal],
            },
          });
        } else {
          await invoke('add_provider', {
            provider: {
              name:    'ollama',
              baseUrl: ollamaBaseUrl,
              apiKey:  null,
              models:  [ollamaModelVal],
            },
          });
        }
        // Refresh local provider list after mutation.
        providers = await invoke('list_providers');
      } catch (err) {
        console.warn('Failed to update ollama provider entry:', err);
        /* non-fatal — proceed to set_settings */
      }

      // ── Build routing object from UI state ─────────────────────────────
      // Start with the Ollama quick-config as the interactive baseline.
      // If the routing tab has explicit values for both provider and model,
      // those take precedence over the Ollama quick-config.
      const routing = {
        interactive: { provider: 'ollama', model: ollamaModelVal },
      };
      // Routing-tab selections override the Ollama baseline when both fields
      // are filled in (provider name alone is not enough).
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
      // `model` and `endpoint` are also updated for legacy / wizard compat.
      await invoke('set_settings', {
        settings: {
          ...fresh,
          model:           ollamaModelVal,
          endpoint:        ollamaBaseUrl,
          autoStart:       prefAutoStart,
          activationHotkey: prefActivationHotkey.trim() || 'Ctrl+Space',
          systemPrompt:    prefSystemPrompt,
          routing,
          channelAdapters: channelAdapters,
          mcpServers:      mcpServers,
          preferences: {
            agentName:            prefAgentName,
            personalityNotes:     prefPersonalityNotes,
            autoRecall:           prefAutoRecall,
            captureCategories:    prefCaptureCategories,
            notificationsEnabled: prefNotificationsEnabled,
          },
          telemetry: {
            enabled: telemetryEnabled,
            uploadEnabled: telemetryUploadEnabled,
            uploadEndpoint: telemetryUploadEndpoint.trim() || null,
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

  // ── MCP server management ───────────────────────────────────────────────
  function openMcpForm(/** @type {McpServer|null} */ server) {
    if (server) {
      editMcpName = server.name;
      mcpFormName = server.name;
      mcpFormCommand = server.command;
      mcpFormArgs = server.args.join(' ');
      mcpFormEnabled = server.enabled;
    } else {
      editMcpName = null;
      mcpFormName = '';
      mcpFormCommand = '';
      mcpFormArgs = '';
      mcpFormEnabled = true;
    }
    showMcpForm = true;
  }

  function saveMcpServer() {
    const server = {
      name: mcpFormName.trim(),
      command: mcpFormCommand.trim(),
      args: mcpFormArgs.trim() ? mcpFormArgs.trim().split(/\s+/) : [],
      enabled: mcpFormEnabled,
    };
    if (!server.name || !server.command) return;

    if (editMcpName) {
      const idx = mcpServers.findIndex(s => s.name === editMcpName);
      if (idx >= 0) mcpServers[idx] = server;
    } else {
      mcpServers.push(server);
    }
    showMcpForm = false;
  }

  function removeMcpServer(/** @type {string} */ name) {
    mcpServers = mcpServers.filter(s => s.name !== name);
  }

  function toggleMcpServer(/** @type {string} */ name) {
    const s = mcpServers.find(s => s.name === name);
    if (s) s.enabled = !s.enabled;
  }

  async function restartMcp() {
    mcpRestarting = true;
    try {
      await invoke('restart_mcp_servers');
      mcpTools = await invoke('list_mcp_tools');
    } catch (err) {
      alert(`MCP restart failed: ${err}`);
    } finally {
      mcpRestarting = false;
    }
  }

  // ── License ──────────────────────────────────────────────────────────────
  async function activateLicense() {
    const key = licenseKey.trim();
    if (!key) {
      licenseError = 'Please enter a license key.';
      return;
    }
    licenseActivating = true;
    licenseError = '';
    try {
      licenseStatus = await invoke('activate_license', { key });
      licenseKey = '';
    } catch (err) {
      licenseError = `Activation failed: ${err}`;
    } finally {
      licenseActivating = false;
    }
  }

  function telemetryTodayKey() {
    return new Date().toISOString().slice(0, 10);
  }

  function telemetryModelCallsToday() {
    const byDay = telemetrySnapshot.modelCallsByDay ?? {};
    return byDay[telemetryTodayKey()] ?? 0;
  }

  function telemetryToolEntries() {
    return Object.entries(telemetrySnapshot.toolUsageFrequency ?? {}).sort((a, b) => b[1] - a[1]);
  }

  async function uploadTelemetryNow() {
    telemetryUploading = true;
    telemetryUploadError = '';
    try {
      await invoke('upload_telemetry_snapshot');
      telemetrySnapshot = await invoke('get_telemetry_snapshot');
    } catch (err) {
      telemetryUploadError = `Upload failed: ${err}`;
    } finally {
      telemetryUploading = false;
    }
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

      <!-- Ollama quick configure — visible immediately when Settings opens -->
      <div class="pref-section ollama-section">
        <p class="pref-section-title">Ollama</p>
        <label>
          Endpoint URL
          <input
            type="url"
            bind:value={ollamaUrl}
            placeholder="http://localhost:11434"
            aria-label="Ollama endpoint URL" />
        </label>
        <label>
          Model
          <input
            type="text"
            bind:value={ollamaModel}
            placeholder="llama3"
            aria-label="Ollama model name" />
        </label>
        <p class="pref-hint">
          Changes are applied on Save. Run <code>ollama pull {ollamaModel || 'llama3'}</code> to
          ensure the model is available locally.
        </p>
      </div>

      <hr class="section-divider" aria-hidden="true" />

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
              placeholder="http://localhost:11434" />
          </label>
          <label>
            API Key <span class="pref-hint">{editProviderName !== null ? '(leave blank to keep existing)' : '(leave blank for local models)'}</span>
            <input type="password" bind:value={providerFormKey}
              placeholder={editProviderName !== null ? 'unchanged' : 'sk-…'} autocomplete="off" />
          </label>
          <label>
            Models <span class="pref-hint">(comma-separated)</span>
            <input type="text" bind:value={providerFormModels}
              placeholder="llama3, llama3.1:8b" />
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
        <label>
          Activation hotkey
          <input type="text" bind:value={prefActivationHotkey} placeholder="Ctrl+Space" />
        </label>
      </div>
    </div>

    <!-- MCP panel -->
    <div
      role="tabpanel"
      id="panel-mcp"
      aria-labelledby="tab-mcp"
      class="settings-panel"
      hidden={activeTab !== 'mcp'}>

      <div class="mcp-header">
        <h3 class="panel-title">MCP Servers</h3>
        <div class="mcp-header-actions">
          <button type="button" class="btn-sm" onclick={() => openMcpForm(null)}>
            + Add Server
          </button>
          <button type="button" class="btn-sm btn-secondary" onclick={restartMcp} disabled={mcpRestarting}>
            {mcpRestarting ? '↻ Restarting…' : '↻ Restart All'}
          </button>
        </div>
      </div>

      {#if showMcpForm}
        <div class="mcp-form">
          <div class="form-row">
            <label class="form-label">
              Name
              <input type="text" bind:value={mcpFormName} placeholder="e.g. filesystem" class="form-input" />
            </label>
          </div>
          <div class="form-row">
            <label class="form-label">
              Command
              <input type="text" bind:value={mcpFormCommand} placeholder="e.g. uvx, npx, node" class="form-input" />
            </label>
          </div>
          <div class="form-row">
            <label class="form-label">
              Arguments
              <input type="text" bind:value={mcpFormArgs} placeholder="e.g. mcp-server-filesystem /tmp" class="form-input" />
            </label>
          </div>
          <div class="form-row">
            <label class="checkbox-item">
              <input type="checkbox" bind:checked={mcpFormEnabled} />
              Enabled
            </label>
          </div>
          <div class="form-actions">
            <button type="button" class="btn-sm" onclick={saveMcpServer}>
              {editMcpName ? 'Update' : 'Add'}
            </button>
            <button type="button" class="btn-sm btn-secondary" onclick={() => { showMcpForm = false; }}>
              Cancel
            </button>
          </div>
        </div>
      {/if}

      {#if mcpServers.length === 0}
        <p class="empty-state">No MCP servers configured. Add one to enable tool use.</p>
      {:else}
        <div class="mcp-server-list">
          {#each mcpServers as server}
            <div class="mcp-server-card" class:disabled={!server.enabled}>
              <div class="mcp-server-info">
                <span class="mcp-server-name">{server.name}</span>
                <code class="mcp-server-cmd">{server.command} {server.args.join(' ')}</code>
              </div>
              <div class="mcp-server-actions">
                <button type="button" class="btn-icon" onclick={() => toggleMcpServer(server.name)}
                  title={server.enabled ? 'Disable' : 'Enable'}>
                  {server.enabled ? '🟢' : '⚪'}
                </button>
                <button type="button" class="btn-icon" onclick={() => openMcpForm(server)} title="Edit">
                  ✏️
                </button>
                <button type="button" class="btn-icon btn-danger" onclick={() => removeMcpServer(server.name)} title="Remove">
                  🗑
                </button>
              </div>
            </div>
          {/each}
        </div>
      {/if}

      {#if mcpTools.length > 0}
        <div class="mcp-tools-section">
          <h4 class="mcp-tools-title">Discovered Tools ({mcpTools.length})</h4>
          <div class="mcp-tools-list">
            {#each mcpTools as tool}
              <div class="mcp-tool-item">
                <span class="mcp-tool-name">{tool.name}</span>
                <span class="mcp-tool-server">{tool.serverName}</span>
                {#if tool.description}
                  <span class="mcp-tool-desc">{tool.description}</span>
                {/if}
              </div>
            {/each}
          </div>
        </div>
      {/if}
    </div>

    <!-- Telemetry panel -->
    <div
      role="tabpanel"
      id="panel-telemetry"
      aria-labelledby="tab-telemetry"
      class="settings-panel"
      hidden={activeTab !== 'telemetry'}>

      <div class="pref-section">
        <p class="pref-section-title">Privacy-first telemetry</p>
        <p class="pref-hint">Anonymous metrics only. No conversation content, prompts, tool arguments, or personal identifiers are collected.</p>

        <div class="pref-toggle-row">
          <div class="pref-toggle-text">
            <span class="pref-label">Enable telemetry (opt-in)</span>
            <span class="pref-hint">Off by default</span>
          </div>
          <label class="toggle" aria-label="Enable anonymous telemetry">
            <input class="toggle-input" type="checkbox" bind:checked={telemetryEnabled} />
            <span class="toggle-slider" aria-hidden="true"></span>
          </label>
        </div>

        <div class="pref-toggle-row">
          <div class="pref-toggle-text">
            <span class="pref-label">Enable upload</span>
            <span class="pref-hint">Manual upload of local aggregate metrics</span>
          </div>
          <label class="toggle" aria-label="Enable telemetry upload">
            <input class="toggle-input" type="checkbox" bind:checked={telemetryUploadEnabled} disabled={!telemetryEnabled} />
            <span class="toggle-slider" aria-hidden="true"></span>
          </label>
        </div>

        <label>
          Upload endpoint
          <input
            type="url"
            bind:value={telemetryUploadEndpoint}
            placeholder="https://example.com/telemetry"
            disabled={!telemetryEnabled || !telemetryUploadEnabled}
            aria-label="Telemetry upload endpoint" />
        </label>
        <button
          type="button"
          class="btn-secondary"
          onclick={uploadTelemetryNow}
          disabled={!telemetryEnabled || !telemetryUploadEnabled || !telemetryUploadEndpoint.trim() || telemetryUploading}>
          {telemetryUploading ? 'Uploading…' : 'Upload now'}
        </button>
        {#if telemetryUploadError}
          <p class="upgrade-error" role="alert">{telemetryUploadError}</p>
        {/if}
      </div>

      <div class="pref-section">
        <p class="pref-section-title">Local telemetry dashboard</p>
        <div class="routing-row">
          <span class="routing-label">Model calls today</span>
          <strong>{telemetryModelCallsToday()}</strong>
        </div>
        <div class="routing-row">
          <span class="routing-label">Avg response latency</span>
          <strong>{telemetrySnapshot.avgLatencyMs == null ? '—' : `${Math.round(telemetrySnapshot.avgLatencyMs)} ms`}</strong>
        </div>
        <div class="routing-row">
          <span class="routing-label">Latency range</span>
          <strong>{telemetrySnapshot.latencyMinMs == null ? '—' : `${telemetrySnapshot.latencyMinMs}–${telemetrySnapshot.latencyMaxMs} ms`}</strong>
        </div>
        <div class="routing-row">
          <span class="routing-label">Samples</span>
          <strong>{telemetrySnapshot.latencySampleCount ?? 0}</strong>
        </div>
        <div class="routing-row">
          <span class="routing-label">Last upload</span>
          <strong>{telemetrySnapshot.lastUploadAt ? new Date(telemetrySnapshot.lastUploadAt).toLocaleString() : 'Never'}</strong>
        </div>

        <hr class="section-divider" aria-hidden="true" />
        <p class="pref-hint">Tool usage frequency</p>
        {#if telemetryToolEntries().length === 0}
          <p class="panel-empty">No tool usage recorded yet.</p>
        {:else}
          <table class="provider-table">
            <thead>
              <tr>
                <th>Tool</th>
                <th>Calls</th>
              </tr>
            </thead>
            <tbody>
              {#each telemetryToolEntries() as [toolName, count] (toolName)}
                <tr>
                  <td class="provider-name">{toolName}</td>
                  <td>{count}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </div>
    </div>

    <!-- License panel -->
    <div
      role="tabpanel"
      id="panel-license"
      aria-labelledby="tab-license"
      class="settings-panel"
      hidden={activeTab !== 'license'}>

      <div class="license-status-row">
        <span
          class="license-badge"
          class:license-pro={licenseStatus.tier === 'pro' && licenseStatus.valid}
          class:license-free={!(licenseStatus.tier === 'pro' && licenseStatus.valid)}>
          {licenseStatus.tier === 'pro' && licenseStatus.valid ? 'Pro' : 'Free'}
        </span>
        {#if licenseStatus.expires_at}
          <span class="pref-hint">Expires: {new Date(licenseStatus.expires_at).toLocaleDateString()}</span>
        {/if}
      </div>

      {#if !(licenseStatus.tier === 'pro' && licenseStatus.valid)}
        <div class="upgrade-features">
          <p>Unlock the full power of Pares Agens:</p>
          <ul class="feature-list">
            <li>✅ Multiple channel adapters</li>
            <li>✅ Multi-provider model routing</li>
            <li>✅ Hyperswarm P2P sync</li>
            <li>✅ MCP tool orchestration</li>
            <li>✅ Praxis audit export</li>
            <li>✅ Procedure editor</li>
          </ul>
        </div>

        <div class="upgrade-activate">
          <label for="license-key-input">License Key</label>
          <input
            id="license-key-input"
            type="text"
            class="license-key-input"
            bind:value={licenseKey}
            placeholder="XXXX-XXXX-XXXX-XXXX"
            autocomplete="off"
            aria-label="License key"
            onkeydown={(e) => { if (e.key === 'Enter') activateLicense(); }}
          />
          {#if licenseError}
            <p class="upgrade-error" role="alert">{licenseError}</p>
          {/if}
          <button
            type="button"
            class="btn-primary"
            onclick={activateLicense}
            disabled={licenseActivating}>
            {licenseActivating ? 'Activating…' : 'Activate'}
          </button>
        </div>
      {:else}
        <p class="pref-hint" style="margin-top: 12px;">Pro features are active. Thank you for your support!</p>
      {/if}
    </div>

    <!-- Marketplace panel -->
    <div
      role="tabpanel"
      id="panel-marketplace"
      aria-labelledby="tab-marketplace"
      class="settings-panel"
      hidden={activeTab !== 'marketplace'}>
      <MarketplaceTab />
    </div>

    <footer class="dialog-footer">
      <button type="button" onclick={() => { open = false; }}>Cancel</button>
      <button type="button" class="btn-primary" onclick={saveAll}>Save</button>
    </footer>
  </form>
</dialog>
