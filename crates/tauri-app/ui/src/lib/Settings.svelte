<script>
  const { invoke } = window.__TAURI__.core;

  let { open = $bindable(false) } = $props();

  /** @type {HTMLDialogElement} */
  let dialog = $state(null);

  let model        = $state('');
  let endpoint     = $state('');
  let systemPrompt = $state('');
  let channel      = $state('tauri');
  let autoStart    = $state(false);

  $effect(() => {
    if (!dialog) return;
    if (open) {
      invoke('get_settings').then(s => {
        model        = s.model        ?? '';
        endpoint     = s.endpoint     ?? '';
        systemPrompt = s.systemPrompt ?? '';
        channel      = s.channel      ?? 'tauri';
        autoStart    = s.autoStart    ?? false;
      }).catch(() => { /* proceed with current values */ });
      dialog.showModal();
    } else {
      dialog.close();
    }
  });

  async function save() {
    try {
      await invoke('set_settings', {
        settings: { model, endpoint, systemPrompt, channel, autoStart },
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
  <form method="dialog">
    <header class="dialog-header">
      <h2>Settings</h2>
      <button class="icon-btn close-btn" type="button"
        onclick={() => { open = false; }} aria-label="Close settings">✕</button>
    </header>

    <fieldset class="settings-section">
      <legend>Model</legend>
      <label>
        Model ID
        <input type="text" bind:value={model} placeholder="qwen3:235b" />
      </label>
      <label>
        Endpoint URL
        <input type="url" bind:value={endpoint} placeholder="http://localhost:11434/v1" />
      </label>
      <label>
        System Prompt
        <textarea bind:value={systemPrompt} rows="3"></textarea>
      </label>
    </fieldset>

    <fieldset class="settings-section">
      <legend>Channel</legend>
      <label>
        Active Channel
        <select bind:value={channel}>
          <option value="tauri">tauri (desktop)</option>
          <option value="stdin">stdin</option>
          <option value="telegram">telegram</option>
        </select>
      </label>
    </fieldset>

    <fieldset class="settings-section">
      <legend>Startup</legend>
      <label class="label-inline">
        <input type="checkbox" bind:checked={autoStart} />
        Launch at login (minimized to tray)
      </label>
    </fieldset>

    <footer class="dialog-footer">
      <button type="button" onclick={() => { open = false; }}>Cancel</button>
      <button type="button" class="btn-primary" onclick={save}>Save</button>
    </footer>
  </form>
</dialog>
