<script>
  /**
   * Settings dialog component.
   *
   * Usage:
   *   <Settings bind:open={settingsOpen} />
   */

  const { invoke } = window.__TAURI__.core;

  /** @type {{ open: boolean }} */
  let { open = $bindable(false) } = $props();

  /** @type {HTMLDialogElement | null} */
  let dialog = $state(null);

  let model        = $state("");
  let endpoint     = $state("");
  let systemPrompt = $state("");
  let channel      = $state("tauri");

  // When the parent sets open=true, load settings and show the dialog.
  $effect(() => {
    if (open && dialog) {
      (async () => {
        try {
          const s = await invoke("get_settings");
          model        = s.model        ?? "";
          endpoint     = s.endpoint     ?? "";
          systemPrompt = s.systemPrompt ?? "";
          channel      = s.channel      ?? "tauri";
        } catch {
          // Use current state values if fetch fails.
        }
        dialog.showModal();
      })();
    }
  });

  function close() {
    dialog?.close();
    open = false;
  }

  async function save() {
    try {
      await invoke("set_settings", {
        settings: { model, endpoint, systemPrompt, channel },
      });
      close();
    } catch (err) {
      alert(`Failed to save settings: ${err}`);
    }
  }

  /** Close on backdrop click. */
  /** @param {MouseEvent} e */
  function onDialogClick(e) {
    if (e.target === dialog) close();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<dialog
  class="settings-dialog"
  aria-label="Settings"
  bind:this={dialog}
  onclick={onDialogClick}
  onkeydown={(e) => { if (e.key === "Escape") close(); }}
>
  <form method="dialog">
    <header class="dialog-header">
      <h2>Settings</h2>
      <button
        class="icon-btn close-btn"
        type="button"
        aria-label="Close settings"
        onclick={() => close()}
      >✕</button>
    </header>

    <fieldset class="settings-section">
      <legend>Model</legend>
      <label>
        Model ID
        <input type="text" name="model" placeholder="qwen3:235b" bind:value={model} />
      </label>
      <label>
        Endpoint URL
        <input type="url" name="endpoint" placeholder="http://localhost:11434/v1" bind:value={endpoint} />
      </label>
      <label>
        System Prompt
        <textarea name="systemPrompt" rows="3" bind:value={systemPrompt}></textarea>
      </label>
    </fieldset>

    <fieldset class="settings-section">
      <legend>Channel</legend>
      <label>
        Active Channel
        <select name="channel" bind:value={channel}>
          <option value="tauri">tauri (desktop)</option>
          <option value="stdin">stdin</option>
          <option value="telegram">telegram</option>
        </select>
      </label>
    </fieldset>

    <footer class="dialog-footer">
      <button type="button" onclick={() => close()}>Cancel</button>
      <button type="button" class="btn-primary" onclick={save}>Save</button>
    </footer>
  </form>
</dialog>

<style>
  .settings-dialog {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: 0 24px 64px rgba(0, 0, 0, 0.6);
    color: var(--text-primary);
    max-width: 520px;
    width: 90%;
    padding: 0;
  }

  .settings-dialog::backdrop {
    background: rgba(0, 0, 0, 0.6);
    backdrop-filter: blur(2px);
  }

  .dialog-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 20px 24px 0;
  }

  .dialog-header h2 { font-size: 16px; font-weight: 600; }

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
  }

  .settings-section label {
    display: flex;
    flex-direction: column;
    gap: 5px;
    font-size: 13px;
    color: var(--text-secondary);
  }

  .settings-section input,
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

  .settings-section input:focus,
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
