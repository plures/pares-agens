<script>
  const invoke = window.__TAURI__?.core?.invoke
    ?? (async () => { throw new Error('Tauri invoke unavailable'); });

  /** @type {{ onComplete: (name: string) => void }} */
  let { onComplete } = $props();

  // ── localStorage keys ────────────────────────────────────────────────────
  const LS_COMPLETED = 'wizard_completed';
  const LS_STATE     = 'wizard_state';

  // ── Constants ────────────────────────────────────────────────────────────
  const DOCKER_MODEL    = 'ai/qwen2.5:latest';
  const DOCKER_ENDPOINT = 'http://localhost:12434/engines/llama.cpp/v1';

  // ── Wizard state ─────────────────────────────────────────────────────────
  let visible       = $state(false);
  let step          = $state(0);     // 0–4

  let agentName     = $state('');
  let modelSource   = $state('');    // '' | 'local' | 'cloud' | 'skip'
  let cloudProvider = $state('openai');
  let apiKey        = $state('');    // in-memory only, never persisted
  let systemPrompt  = $state('');
  let telegramToken = $state('');    // in-memory only, never persisted
  let swarmMode     = $state('skip'); // 'skip' | 'new' | 'join'
  let swarmTopic    = $state('');
  let swarmSharedKey = $state('');    // in-memory only, never persisted

  // ── Detection / validation state ─────────────────────────────────────────
  let dockerStatus  = $state('idle');    // 'idle' | 'checking' | 'found' | 'absent'
  let apiKeyStatus  = $state('idle');    // 'idle' | 'checking' | 'valid' | 'invalid' | 'error'
  let swarmVerifyStatus = $state('idle'); // 'idle' | 'checking' | 'success' | 'error'
  let swarmVerifyError = $state('');

  // ── Summary items ────────────────────────────────────────────────────────
  let summaryItems = $derived([
    { label: 'Agent name', value: agentName || 'Pares Agens' },
    {
      label: 'Model',
      value: modelSource === 'local'  ? 'Docker Model Runner (local)'
           : modelSource === 'cloud'  ? `Cloud — ${cloudProvider}`
           : 'Not configured (configure later in Settings)',
    },
    {
      label: 'Personality',
      value: systemPrompt ? 'Custom system prompt set' : 'Default',
    },
    {
      label: 'Channel',
      value: telegramToken ? 'Telegram connected' : 'Desktop only',
    },
    {
      label: 'Swarm',
      value: swarmMode === 'new'
        ? 'New swarm generated'
        : swarmMode === 'join'
          ? 'Joining existing swarm'
          : 'Not configured',
    },
  ]);

  // ── Persistence helpers ───────────────────────────────────────────────────
  function saveState() {
    // apiKey, telegramToken, and swarmSharedKey intentionally excluded
    const persistable = {
      step,
      agentName,
      modelSource,
      cloudProvider,
      systemPrompt,
      swarmMode,
      swarmTopic,
    };
    localStorage.setItem(LS_STATE, JSON.stringify(persistable));
  }

  function loadState() {
    try {
      const raw = localStorage.getItem(LS_STATE);
      if (!raw) return;
      const s = JSON.parse(raw);
      agentName     = s.agentName     || '';
      modelSource   = s.modelSource   || '';
      cloudProvider = s.cloudProvider || 'openai';
      systemPrompt  = s.systemPrompt  || '';
      swarmMode     = s.swarmMode     || 'skip';
      swarmTopic    = s.swarmTopic    || '';
      // Clamp step to 0–4
      const loaded  = typeof s.step === 'number' ? s.step : 0;
      step = Math.min(Math.max(loaded, 0), 4);
    } catch {
      step = 0;
    }
  }

  // ── Navigation ────────────────────────────────────────────────────────────
  function goNext() {
    step = Math.min(step + 1, 4);
    saveState();
  }

  function goBack() {
    step = Math.max(step - 1, 0);
    saveState();
  }

  // ── Docker detection ──────────────────────────────────────────────────────
  async function runDockerDetect() {
    dockerStatus = 'checking';
    try {
      const found = await invoke('detect_docker_runner');
      dockerStatus = found ? 'found' : 'absent';
    } catch {
      dockerStatus = 'absent';
    }
  }

  // ── API key validation ────────────────────────────────────────────────────
  async function validateKey() {
    if (!apiKey.trim()) return;
    apiKeyStatus = 'checking';
    try {
      const valid = await invoke('validate_api_key', { provider: cloudProvider, apiKey });
      apiKeyStatus = valid ? 'valid' : 'invalid';
    } catch {
      apiKeyStatus = 'error';
    }
  }

  // ── Swarm setup helpers ───────────────────────────────────────────────────
  async function createSwarmInvite() {
    swarmVerifyStatus = 'idle';
    swarmVerifyError = '';
    try {
      const invite = await invoke('generate_swarm_invite');
      swarmTopic = invite.topic || '';
      swarmSharedKey = invite.sharedKey || '';
    } catch (err) {
      swarmVerifyStatus = 'error';
      swarmVerifyError = `Failed to generate swarm invite: ${String(err)}`;
    }
  }

  async function verifyJoinSwarm() {
    if (!swarmTopic.trim() || !swarmSharedKey.trim()) return;
    swarmVerifyStatus = 'checking';
    swarmVerifyError = '';
    try {
      await invoke('verify_swarm_join', {
        topic: swarmTopic.trim(),
        sharedKey: swarmSharedKey.trim(),
      });
      swarmVerifyStatus = 'success';
    } catch (err) {
      swarmVerifyStatus = 'error';
      swarmVerifyError = String(err);
    }
  }

  // ── Finish wizard ─────────────────────────────────────────────────────────
  async function finishWizard() {
    const name = agentName.trim() || 'Pares Agens';

    let model    = 'llama3';
    let endpoint = 'http://localhost:11434';
    let finalKey = null;

    if (modelSource === 'local') {
      model    = DOCKER_MODEL;
      endpoint = DOCKER_ENDPOINT;
    } else if (modelSource === 'cloud') {
      finalKey = apiKey || null;
      switch (cloudProvider) {
        case 'openai':
          model    = 'gpt-4o-mini';
          endpoint = 'https://api.openai.com/v1';
          break;
        case 'anthropic':
          model    = 'claude-3-5-haiku-20241022';
          endpoint = 'https://api.anthropic.com/v1';
          break;
        case 'google':
          model    = 'gemini-1.5-flash';
          endpoint = 'https://generativelanguage.googleapis.com/v1beta';
          break;
      }
    }

    const resolvedPrompt = systemPrompt.trim()
      || `You are ${name}, a helpful desktop AI assistant.`;

    const channel = telegramToken ? 'telegram' : 'tauri';

    /** @type {Record<string, unknown>} */
    const settings = {
      model,
      endpoint,
      channel,
      systemPrompt: resolvedPrompt,
      autoStart: false,
    };
    if (finalKey)      settings.apiKey        = finalKey;
    if (telegramToken) settings.telegramToken = telegramToken;
    const swarm = swarmMode === 'skip'
      ? null
      : {
          mode: swarmMode,
          topic: swarmTopic.trim(),
          sharedKey: swarmSharedKey.trim(),
        };

    try {
      await invoke('complete_wizard', { settings, swarm });
    } catch (err) {
      console.error('complete_wizard failed, falling back to set_settings:', err);
      try { await invoke('set_settings', { settings }); } catch (e) {
        console.error('set_settings fallback also failed:', e);
      }
    }

    localStorage.setItem(LS_COMPLETED, '1');
    localStorage.removeItem(LS_STATE);

    visible = false;
    onComplete(name);
  }

  // ── Initialise ────────────────────────────────────────────────────────────
  $effect(() => {
    if (localStorage.getItem(LS_COMPLETED)) {
      visible = false;
      return;
    }
    loadState();
    visible = true;
    // Kick off Docker probe in background so result is ready when user reaches step 1
    runDockerDetect();
  });

  // Reset API key validation status whenever provider or key changes
  $effect(() => {
    void cloudProvider;
    void apiKey;
    apiKeyStatus = 'idle';
  });

  $effect(() => {
    void swarmTopic;
    void swarmSharedKey;
    if (swarmVerifyStatus === 'error') {
      swarmVerifyStatus = 'idle';
      swarmVerifyError = '';
    }
  });

  $effect(() => {
    void swarmMode;
    if (swarmMode === 'new' && (!swarmTopic || !swarmSharedKey)) {
      createSwarmInvite();
    }
  });
</script>

{#if visible}
<div
  class="wizard-overlay"
  role="dialog"
  aria-modal="true"
  aria-label="Setup Wizard"
>
  <div class="wizard-card">
    <!-- Progress bar -->
    <div class="wizard-progress" role="progressbar" aria-valuenow={step + 1} aria-valuemin="1" aria-valuemax="5">
      {#each [0, 1, 2, 3, 4] as i}
        <div class="wizard-progress-step {i <= step ? 'active' : ''}"></div>
      {/each}
    </div>

    <!-- ── Step 0 — Agent Name ─────────────────────────────────────────── -->
    {#if step === 0}
      <div class="wizard-step" aria-live="polite">
        <h2 class="wizard-title">Welcome to Pares Agens</h2>
        <p class="wizard-desc">Let's get you set up. First, what should your agent be called?</p>
        <label class="wizard-label">
          Agent name
          <input
            class="wizard-input"
            type="text"
            placeholder="Pares Agens"
            maxlength="64"
            bind:value={agentName}
            onkeydown={(e) => { if (e.key === 'Enter') { saveState(); goNext(); } }}
          />
        </label>
        <div class="wizard-footer">
          <span></span>
          <button class="btn-primary" onclick={() => { saveState(); goNext(); }}>
            Next →
          </button>
        </div>
      </div>
    {/if}

    <!-- ── Step 1 — Model ─────────────────────────────────────────────── -->
    {#if step === 1}
      <div class="wizard-step" aria-live="polite">
        <h2 class="wizard-title">Choose your model</h2>
        <p class="wizard-desc">How do you want to run your AI models?</p>

        <div class="model-cards">
          <!-- Local / Docker -->
          <label class="model-card {modelSource === 'local' ? 'selected' : ''}">
            <input type="radio" name="model-source" value="local" bind:group={modelSource} class="sr-only" />
            <span class="model-card-title">🐳 Local (Docker)</span>
            <span class="model-card-desc">Run models privately on your machine via Docker Model Runner. No API key needed.</span>
            {#if modelSource === 'local'}
              <span class="detection-badge {dockerStatus === 'found' ? 'badge-ok' : dockerStatus === 'checking' ? 'badge-checking' : 'badge-warn'}">
                {dockerStatus === 'found'    ? '✓ Docker runner detected'
                 : dockerStatus === 'checking' ? '⌛ Checking…'
                 : '⚠ Docker runner not found — start it first'}
              </span>
            {/if}
          </label>

          <!-- Cloud -->
          <label class="model-card {modelSource === 'cloud' ? 'selected' : ''}">
            <input type="radio" name="model-source" value="cloud" bind:group={modelSource} class="sr-only" />
            <span class="model-card-title">☁ Cloud provider</span>
            <span class="model-card-desc">Use OpenAI, Anthropic, or Google. Requires an API key.</span>
          </label>

          <!-- Skip -->
          <label class="model-card {modelSource === 'skip' ? 'selected' : ''}">
            <input type="radio" name="model-source" value="skip" bind:group={modelSource} class="sr-only" />
            <span class="model-card-title">⏭ Configure later</span>
            <span class="model-card-desc">Skip for now — change model settings any time.</span>
          </label>
        </div>

        <!-- Cloud sub-form -->
        {#if modelSource === 'cloud'}
          <div class="cloud-config">
            <label class="wizard-label">
              Provider
              <select class="wizard-select" bind:value={cloudProvider}>
                <option value="openai">OpenAI</option>
                <option value="anthropic">Anthropic</option>
                <option value="google">Google</option>
              </select>
            </label>
            <label class="wizard-label">
              API Key
              <div class="api-key-row">
                <input
                  class="wizard-input"
                  type="password"
                  placeholder="sk-…"
                  bind:value={apiKey}
                />
                <button class="btn-secondary" onclick={validateKey} disabled={!apiKey.trim() || apiKeyStatus === 'checking'}>
                  {apiKeyStatus === 'checking' ? '⌛' : 'Verify'}
                </button>
              </div>
              {#if apiKeyStatus === 'valid'}
                <span class="key-status ok">✓ Key valid</span>
              {:else if apiKeyStatus === 'invalid'}
                <span class="key-status err">✗ Invalid key</span>
              {:else if apiKeyStatus === 'error'}
                <span class="key-status warn">⚠ Provider error — try again</span>
              {/if}
            </label>
          </div>
        {/if}

        <div class="wizard-footer">
          <button class="btn-secondary" onclick={goBack}>← Back</button>
          <button class="btn-primary" onclick={() => { saveState(); goNext(); }} disabled={!modelSource}>
            Next →
          </button>
        </div>
      </div>
    {/if}

    <!-- ── Step 2 — Personality ────────────────────────────────────────── -->
    {#if step === 2}
      <div class="wizard-step" aria-live="polite">
        <h2 class="wizard-title">Personality</h2>
        <p class="wizard-desc">Optionally customise your agent's system prompt. Leave blank for the default.</p>
        <label class="wizard-label">
          System prompt
          <textarea
            class="wizard-textarea"
            rows="5"
            placeholder={`You are ${agentName.trim() || 'Pares Agens'}, a helpful desktop AI assistant.`}
            bind:value={systemPrompt}
          ></textarea>
        </label>
        <div class="wizard-footer">
          <button class="btn-secondary" onclick={goBack}>← Back</button>
          <button class="btn-primary" onclick={() => { saveState(); goNext(); }}>Next →</button>
        </div>
      </div>
    {/if}

    <!-- ── Step 3 — Hyperswarm ─────────────────────────────────────────── -->
    {#if step === 3}
      <div class="wizard-step" aria-live="polite">
        <h2 class="wizard-title">Sync across hosts (optional)</h2>
        <p class="wizard-desc">Set up Hyperswarm now or skip and configure later.</p>

        <div class="model-cards">
          <label class="model-card {swarmMode === 'new' ? 'selected' : ''}">
            <input type="radio" name="swarm-mode" value="new" bind:group={swarmMode} class="sr-only" />
            <span class="model-card-title">✨ New swarm</span>
            <span class="model-card-desc">Generate a new topic + key and share with other hosts.</span>
          </label>
          <label class="model-card {swarmMode === 'join' ? 'selected' : ''}">
            <input type="radio" name="swarm-mode" value="join" bind:group={swarmMode} class="sr-only" />
            <span class="model-card-title">🔗 Join existing swarm</span>
            <span class="model-card-desc">Enter a topic + key from another host and verify them.</span>
          </label>
          <label class="model-card {swarmMode === 'skip' ? 'selected' : ''}">
            <input type="radio" name="swarm-mode" value="skip" bind:group={swarmMode} class="sr-only" />
            <span class="model-card-title">⏭ Skip for now</span>
            <span class="model-card-desc">Continue without Hyperswarm setup.</span>
          </label>
        </div>

        {#if swarmMode === 'new'}
          <div class="cloud-config">
            <button class="btn-secondary" onclick={createSwarmInvite}>
              {swarmTopic && swarmSharedKey ? 'Regenerate topic + key' : 'Generate topic + key'}
            </button>
            {#if swarmTopic && swarmSharedKey}
              <p class="wizard-desc">
                Share this with other hosts:
                <br />
                <strong>Topic:</strong> <code>{swarmTopic}</code>
                <br />
                <strong>Key:</strong> <code>{swarmSharedKey}</code>
              </p>
            {/if}
          </div>
        {:else if swarmMode === 'join'}
          <div class="cloud-config">
            <label class="wizard-label">
              Topic (64 hex chars)
              <input class="wizard-input" type="text" placeholder="a7f3..." bind:value={swarmTopic} />
            </label>
            <label class="wizard-label">
              Shared key
              <input class="wizard-input" type="password" placeholder="b92c..." bind:value={swarmSharedKey} />
            </label>
            <button class="btn-secondary" onclick={verifyJoinSwarm} disabled={!swarmTopic.trim() || !swarmSharedKey.trim() || swarmVerifyStatus === 'checking'}>
              {swarmVerifyStatus === 'checking' ? 'Verifying…' : 'Verify join'}
            </button>
            {#if swarmVerifyStatus === 'success'}
              <span class="key-status ok">✓ Verified — topic + key are valid</span>
            {:else if swarmVerifyStatus === 'error' && swarmVerifyError}
              <span class="key-status err">{swarmVerifyError}</span>
            {/if}
          </div>
        {/if}

        <div class="wizard-footer">
          <button class="btn-secondary" onclick={goBack}>← Back</button>
          <button
            class="btn-primary"
            onclick={() => { saveState(); goNext(); }}
            disabled={swarmMode === 'new' ? (!swarmTopic || !swarmSharedKey) : (swarmMode === 'join' ? swarmVerifyStatus !== 'success' : false)}
          >
            Next →
          </button>
        </div>
      </div>
    {/if}

    <!-- ── Step 4 — Done / Summary ─────────────────────────────────────── -->
    {#if step === 4}
      <div class="wizard-step" aria-live="polite">
        <h2 class="wizard-title">You're all set!</h2>
        <p class="wizard-desc" id="wizard-done-summary">
          {agentName.trim() || 'Pares Agens'} is ready.
        </p>
        <ul class="wizard-summary-list">
          {#each summaryItems as item}
            <li class="wizard-summary-item">
              <span class="wizard-summary-label">{item.label}:</span>
              {item.value}
            </li>
          {/each}
        </ul>
        <div class="wizard-footer">
          <button class="btn-secondary" onclick={goBack}>← Back</button>
          <button class="btn-primary" onclick={finishWizard}>Launch →</button>
        </div>
      </div>
    {/if}
  </div>
</div>
{/if}

<style>
  .wizard-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.75);
    backdrop-filter: blur(4px);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }

  .wizard-card {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: 0 32px 80px rgba(0, 0, 0, 0.7);
    width: min(540px, 94vw);
    max-height: 90vh;
    overflow-y: auto;
    padding: 32px;
    display: flex;
    flex-direction: column;
    gap: 24px;
  }

  /* Progress bar */
  .wizard-progress {
    display: flex;
    gap: 6px;
  }

  .wizard-progress-step {
    flex: 1;
    height: 4px;
    border-radius: 2px;
    background: var(--border);
    transition: background var(--transition);
  }

  .wizard-progress-step.active {
    background: var(--accent);
  }

  /* Step container */
  .wizard-step {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .wizard-title {
    font-size: 20px;
    font-weight: 700;
    color: var(--text-primary);
  }

  .wizard-desc {
    font-size: 14px;
    color: var(--text-secondary);
    line-height: 1.5;
  }

  .wizard-label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 13px;
    color: var(--text-secondary);
  }

  .wizard-input,
  .wizard-select,
  .wizard-textarea {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    font-family: var(--font-sans);
    font-size: 14px;
    padding: 9px 12px;
    outline: none;
    transition: border-color var(--transition);
    width: 100%;
  }

  .wizard-input:focus,
  .wizard-select:focus,
  .wizard-textarea:focus {
    border-color: var(--accent);
  }

  .wizard-textarea { resize: vertical; min-height: 80px; }

  /* Model cards */
  .model-cards {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .model-card {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 14px 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    cursor: pointer;
    transition: border-color var(--transition), background var(--transition);
  }

  .model-card:hover { background: var(--bg-hover); }

  .model-card.selected {
    border-color: var(--accent);
    background: var(--accent-dim);
  }

  .model-card-title {
    font-size: 14px;
    font-weight: 600;
    color: var(--text-primary);
  }

  .model-card-desc {
    font-size: 13px;
    color: var(--text-secondary);
  }

  /* Visually-hidden radio inputs */
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
    border: 0;
  }

  .model-card:focus-within {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  /* Detection badge */
  .detection-badge {
    margin-top: 4px;
    font-size: 12px;
    padding: 3px 8px;
    border-radius: 4px;
    align-self: flex-start;
  }

  .badge-ok      { background: rgba(76, 175, 130, 0.15); color: var(--success); }
  .badge-warn    { background: rgba(224, 92, 92, 0.12);  color: var(--danger); }
  .badge-checking { background: var(--bg-elevated); color: var(--text-muted); }

  /* Cloud config sub-form */
  .cloud-config {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 14px;
    background: var(--bg-elevated);
    border-radius: var(--radius-sm);
    border: 1px solid var(--border);
  }

  .api-key-row {
    display: flex;
    gap: 8px;
  }

  .api-key-row .wizard-input { flex: 1; }

  .key-status {
    font-size: 12px;
    margin-top: 4px;
  }

  .key-status.ok   { color: var(--success); }
  .key-status.err  { color: var(--danger); }
  .key-status.warn { color: #f7a27c; }

  code {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 1px 4px;
    word-break: break-all;
  }

  /* Summary */
  .wizard-summary-list {
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 8px;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 14px 16px;
  }

  .wizard-summary-item {
    font-size: 13px;
    color: var(--text-secondary);
  }

  .wizard-summary-label {
    font-weight: 600;
    color: var(--text-primary);
    margin-right: 4px;
  }

  /* Footer nav */
  .wizard-footer {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding-top: 8px;
  }

  /* Buttons (reuse global design tokens) */
  .btn-primary {
    background: var(--accent);
    border: 1px solid var(--accent);
    border-radius: var(--radius-sm);
    color: #fff;
    cursor: pointer;
    font-size: 13px;
    padding: 8px 20px;
    transition: background var(--transition);
  }

  .btn-primary:hover:not(:disabled)  { background: var(--accent-hover); }
  .btn-primary:disabled { opacity: 0.45; cursor: not-allowed; }

  .btn-secondary {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--text-secondary);
    cursor: pointer;
    font-size: 13px;
    padding: 8px 16px;
    transition: background var(--transition), color var(--transition);
  }

  .btn-secondary:hover:not(:disabled)  { background: var(--bg-hover); color: var(--text-primary); }
  .btn-secondary:disabled { opacity: 0.45; cursor: not-allowed; }
</style>
