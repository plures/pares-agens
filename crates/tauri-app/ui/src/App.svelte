<script>
  /**
   * App — root layout component.
   *
   * Layout: [MemorySidebar | Chat] with a settings button in the header.
   * Responsive: sidebar collapses below 640 px.
   * Theme: dark (default) / light, persisted via PluresDB state (set_settings).
   */
  import { onMount } from 'svelte';
  import Chat from './lib/Chat.svelte';
  import MemorySidebar from './lib/MemorySidebar.svelte';
  import Settings from './lib/Settings.svelte';

  // ── State ──────────────────────────────────────────────────────────────────
  let memories = $state([]);
  let settingsOpen = $state(false);
  let settings = $state({
    model: 'qwen3:235b',
    endpoint: 'http://localhost:11434/v1',
    channel: 'tauri',
    systemPrompt: 'You are Pares Agens, a helpful desktop AI assistant.',
    theme: 'dark',
    viMode: false,
  });

  // ── Tauri IPC ──────────────────────────────────────────────────────────────
  const tauri = globalThis.__TAURI__;
  const invoke = tauri?.core?.invoke ?? tauri?.invoke ?? null;

  // ── Theme ──────────────────────────────────────────────────────────────────
  $effect(() => {
    document.documentElement.setAttribute('data-theme', settings.theme);
  });

  // ── Init ───────────────────────────────────────────────────────────────────
  onMount(async () => {
    if (invoke) {
      try {
        const s = await invoke('get_settings');
        settings = { ...settings, ...s };
      } catch (_) { /* use defaults */ }
      refreshMemories();
    }
  });

  async function refreshMemories() {
    if (!invoke) return;
    try {
      memories = (await invoke('get_memories')) ?? [];
    } catch (_) { /* non-critical */ }
  }

  async function handleSaveSettings(updated) {
    settings = updated;
    settingsOpen = false;
    if (invoke) {
      try {
        await invoke('set_settings', { settings: updated });
      } catch (_) { /* non-critical */ }
    }
  }

  function handleCloseSettings() {
    settingsOpen = false;
  }
</script>

<div class="app-shell" data-theme={settings.theme}>
  <!-- ── Memory Sidebar ───────────────────────────────────────────────────── -->
  <MemorySidebar {memories} />

  <!-- ── Chat + Header wrapper ──────────────────────────────────────────── -->
  <div class="main-area">
    <!-- Global header (settings button lives here so it persists across chat) -->
    <div class="global-header-actions">
      <button
        class="icon-btn"
        title="Settings"
        aria-label="Open settings"
        aria-haspopup="dialog"
        onclick={() => (settingsOpen = true)}
      >⚙</button>
      <button
        class="icon-btn theme-btn"
        title="Toggle theme"
        aria-label="Toggle dark/light theme"
        onclick={() => (settings = { ...settings, theme: settings.theme === 'dark' ? 'light' : 'dark' })}
      >{settings.theme === 'dark' ? '☀' : '🌙'}</button>
    </div>

    <Chat viMode={settings.viMode} />
  </div>
</div>

<!-- ── Settings Dialog ──────────────────────────────────────────────────── -->
<Settings
  open={settingsOpen}
  {settings}
  onclose={handleCloseSettings}
  onsave={handleSaveSettings}
/>

<style>
  /* ── Design-Dojo tokens ─────────────────────────────────────────────────── */
  :global(:root), :global([data-theme="dark"]) {
    --bg-base:        #0e0f11;
    --bg-surface:     #16181c;
    --bg-elevated:    #1e2128;
    --bg-hover:       #262930;
    --border:         #2c2f38;
    --text-primary:   #e8eaf0;
    --text-secondary: #8b90a0;
    --text-muted:     #555a6a;
    --accent:         #7c6af7;
    --accent-hover:   #9480ff;
    --accent-dim:     #2d2660;
    --user-bubble:    #252040;
    --agent-bubble:   #1a1d24;
    --success:        #4caf82;
    --danger:         #e05c5c;
    --radius-sm:      6px;
    --radius-md:      12px;
    --radius-lg:      20px;
    --font-sans:      'Inter', system-ui, -apple-system, sans-serif;
    --font-mono:      'JetBrains Mono', 'Fira Code', monospace;
    --transition:     150ms ease;
  }

  :global([data-theme="light"]) {
    --bg-base:        #f4f5f7;
    --bg-surface:     #ffffff;
    --bg-elevated:    #eceef2;
    --bg-hover:       #e0e3ea;
    --border:         #d0d5e0;
    --text-primary:   #1a1c22;
    --text-secondary: #5a6070;
    --text-muted:     #9098aa;
    --accent:         #5b4de8;
    --accent-hover:   #7060f5;
    --accent-dim:     #dddafc;
    --user-bubble:    #e8e5fc;
    --agent-bubble:   #ffffff;
    --success:        #2e8c5a;
    --danger:         #c94040;
  }

  /* ── Reset ──────────────────────────────────────────────────────────────── */
  :global(*, *::before, *::after) { box-sizing: border-box; margin: 0; padding: 0; }
  :global(html), :global(body) { height: 100%; overflow: hidden; }
  :global(body) {
    background: var(--bg-base);
    color: var(--text-primary);
    font-family: var(--font-sans);
    font-size: 14px;
    line-height: 1.6;
  }
  :global(* ) { scrollbar-width: thin; scrollbar-color: var(--border) transparent; }

  /* ── Layout ─────────────────────────────────────────────────────────────── */
  .app-shell {
    display: grid;
    grid-template-columns: 240px 1fr;
    height: 100vh;
    overflow: hidden;
  }

  .main-area {
    display: flex;
    flex-direction: column;
    overflow: hidden;
    position: relative;
  }

  /* Settings / theme buttons float over the chat header */
  .global-header-actions {
    position: absolute;
    top: 8px;
    right: 12px;
    display: flex;
    gap: 4px;
    z-index: 10;
  }

  .icon-btn {
    background: transparent;
    border: none;
    color: var(--text-secondary);
    cursor: pointer;
    font-size: 16px;
    padding: 6px 8px;
    border-radius: var(--radius-sm);
    transition: background var(--transition), color var(--transition);
    line-height: 1;
  }

  .icon-btn:hover { background: var(--bg-hover); color: var(--text-primary); }

  /* ── Responsive ─────────────────────────────────────────────────────────── */
  @media (max-width: 640px) {
    .app-shell {
      grid-template-columns: 1fr;
    }

    /* Hide the sidebar on very small screens */
    .app-shell > :global(.sidebar) {
      display: none;
    }
  }
</style>
