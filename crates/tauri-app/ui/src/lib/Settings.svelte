<script>
  /**
   * Settings — modal dialog for user-configurable options.
   *
   * Props:
   *   open      {boolean}               — controls dialog visibility
   *   settings  {object}                — current settings values
   *   onclose   {() => void}            — called when dialog is dismissed
   *   onsave    {(settings) => void}    — called with updated settings
   */
  import { untrack } from 'svelte';

  const { open = false, settings = {}, onclose, onsave } = $props();

  let local = $state(untrack(() => ({ ...settings })));

  // Sync local copy when the parent settings prop changes (e.g. initial load)
  $effect(() => {
    local = { ...settings };
  });

  function handleSave() {
    onsave?.(local);
  }

  function handleCancel() {
    local = { ...settings };
    onclose?.();
  }

  /** Close on backdrop click */
  function onDialogClick(e) {
    if (e.target === e.currentTarget) handleCancel();
  }
</script>

{#if open}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="dialog-backdrop"
    role="presentation"
    onclick={onDialogClick}
    aria-hidden="true"
  ></div>
  <dialog
    class="settings-dialog"
    aria-label="Settings"
    aria-modal="true"
    open
  >
    <header class="dialog-header">
      <h2>Settings</h2>
      <button class="icon-btn close-btn" onclick={handleCancel} aria-label="Close settings">✕</button>
    </header>

    <fieldset class="settings-section">
      <legend>Model</legend>
      <label>
        Model ID
        <input type="text" bind:value={local.model} placeholder="qwen3:235b" />
      </label>
      <label>
        Endpoint URL
        <input type="url" bind:value={local.endpoint} placeholder="http://localhost:11434/v1" />
      </label>
      <label>
        System Prompt
        <textarea bind:value={local.systemPrompt} rows="3"></textarea>
      </label>
    </fieldset>

    <fieldset class="settings-section">
      <legend>Channel</legend>
      <label>
        Active Channel
        <select bind:value={local.channel}>
          <option value="tauri">tauri (desktop)</option>
          <option value="stdin">stdin</option>
          <option value="telegram">telegram</option>
        </select>
      </label>
    </fieldset>

    <fieldset class="settings-section">
      <legend>Appearance</legend>
      <label class="row-label">
        <span>Theme</span>
        <select bind:value={local.theme}>
          <option value="dark">Dark</option>
          <option value="light">Light</option>
        </select>
      </label>
      <label class="row-label">
        <span>Vi-style input mode</span>
        <input type="checkbox" bind:checked={local.viMode} />
      </label>
    </fieldset>

    <footer class="dialog-footer">
      <button onclick={handleCancel}>Cancel</button>
      <button class="btn-primary" onclick={handleSave}>Save</button>
    </footer>
  </dialog>
{/if}

<style>
  .dialog-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    backdrop-filter: blur(2px);
    z-index: 100;
  }

  .settings-dialog {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: 0 24px 64px rgba(0, 0, 0, 0.6);
    color: var(--text-primary);
    max-width: 520px;
    width: 90%;
    padding: 0;
    z-index: 101;
    max-height: 90vh;
    overflow-y: auto;
  }

  .dialog-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 20px 24px 0;
  }

  .dialog-header h2 {
    font-size: 16px;
    font-weight: 600;
    margin: 0;
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

  .close-btn { font-size: 14px; }

  .settings-section {
    border: none;
    padding: 20px 24px 0;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  .settings-section legend {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-muted);
    margin-bottom: 12px;
    padding: 0;
    float: left;
    width: 100%;
  }

  .settings-section label {
    display: flex;
    flex-direction: column;
    gap: 5px;
    font-size: 13px;
    color: var(--text-secondary);
  }

  .row-label {
    flex-direction: row !important;
    align-items: center;
    justify-content: space-between;
  }

  .settings-section input[type="text"],
  .settings-section input[type="url"],
  .settings-section textarea,
  .settings-section select {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    font-family: var(--font-sans);
    font-size: 13px;
    padding: 8px 10px;
    outline: none;
    transition: border-color var(--transition);
  }

  .settings-section input[type="text"]:focus,
  .settings-section input[type="url"]:focus,
  .settings-section textarea:focus,
  .settings-section select:focus { border-color: var(--accent); }

  .settings-section textarea { resize: vertical; min-height: 60px; }
  .settings-section select option { background: var(--bg-elevated); }

  .dialog-footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 20px 24px;
  }

  .dialog-footer button {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--text-secondary);
    cursor: pointer;
    font-size: 13px;
    padding: 7px 16px;
    transition: background var(--transition), color var(--transition);
  }

  .dialog-footer button:hover {
    background: var(--bg-hover);
    color: var(--text-primary);
  }

  .btn-primary {
    background: var(--accent) !important;
    border-color: var(--accent) !important;
    color: #fff !important;
  }

  .btn-primary:hover {
    background: var(--accent-hover) !important;
    border-color: var(--accent-hover) !important;
  }
</style>
