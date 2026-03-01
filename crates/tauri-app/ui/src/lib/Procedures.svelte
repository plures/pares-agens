<script>
  const { invoke } = window.__TAURI__.core;

  let { open = $bindable(false) } = $props();

  /** @type {HTMLDialogElement} */
  let dialog = $state(null);

  /**
   * @typedef {{ name: string, eventType: string, priority: number, enabled: boolean, body: string }} ProcRecord
   * @typedef {{ procedureName: string, firedAt: string, durationMs: number, triggerEvent: string }} LogEntry
   */

  /** @type {ProcRecord[]} */
  let procedures = $state([]);
  /** @type {ProcRecord | null} */
  let selected = $state(null);
  /** @type {LogEntry[]} */
  let logEntries = $state([]);
  let editMode = $state(false);
  let editBody = $state('');
  let templateValue = $state('');

  $effect(() => {
    if (!dialog) return;
    if (open) {
      selected = null;
      editMode = false;
      editBody = '';
      logEntries = [];
      refreshProcedures();
      dialog.showModal();
    } else {
      dialog.close();
    }
  });

  async function refreshProcedures() {
    try {
      procedures = await invoke('list_procedures');
    } catch {
      procedures = [];
    }
  }

  async function selectProcedure(name) {
    try {
      const rec = await invoke('get_procedure', { name });
      if (!rec) return;
      selected = rec;
      editBody = rec.body;
      editMode = false;
      await refreshLog(name);
    } catch (err) {
      console.error('selectProcedure error:', err);
    }
  }

  async function refreshLog(name) {
    try {
      logEntries = await invoke('get_procedure_log', { name, limit: 50 });
    } catch {
      logEntries = [];
    }
  }

  async function toggleEnabled(e) {
    if (!selected) return;
    const enabled = e.target.checked;
    try {
      await invoke('toggle_procedure', { name: selected.name, enabled });
      selected = { ...selected, enabled };
      await refreshProcedures();
    } catch (err) {
      e.target.checked = !enabled;
      alert(`Failed to toggle procedure: ${err}`);
    }
  }

  async function saveProcedure() {
    if (!selected) return;
    try {
      const record = { ...selected, body: editBody };
      await invoke('save_procedure', { record });
      selected = record;
      editMode = false;
      await refreshProcedures();
    } catch (err) {
      alert(`Failed to save procedure: ${err}`);
    }
  }

  async function createFromTemplate() {
    if (!templateValue) {
      alert('Please select a template first.');
      return;
    }
    try {
      const rec = await invoke('create_from_template', { template: templateValue });
      templateValue = '';
      await refreshProcedures();
      await selectProcedure(rec.name);
    } catch (err) {
      alert(`Failed to create procedure: ${err}`);
    }
  }

  function handleBackdropClick(e) {
    if (e.target === dialog) open = false;
  }
</script>

<dialog
  bind:this={dialog}
  class="procedures-dialog"
  aria-label="Procedure Editor"
  onclick={handleBackdropClick}>
  <header class="dialog-header">
    <h2>⚡ Procedures</h2>
    <button class="icon-btn close-btn" type="button"
      onclick={() => { open = false; }} aria-label="Close procedure editor">✕</button>
  </header>

  <div class="procedures-body">

    <!-- Left: procedure list -->
    <aside class="proc-list-panel" aria-label="Procedure list">
      <div class="proc-list-toolbar">
        <select bind:value={templateValue} aria-label="Select template" title="Select a template">
          <option value="">— New from template —</option>
          <option value="greeting">greeting</option>
          <option value="scheduled_task">scheduled task</option>
          <option value="approval_gate">approval gate</option>
          <option value="memory_pattern">memory pattern</option>
        </select>
        <button type="button" class="btn-primary"
          onclick={createFromTemplate} aria-label="Create procedure from template">＋</button>
      </div>
      <ul class="proc-list" role="listbox" aria-label="Procedures">
        {#if procedures.length === 0}
          <li style="color:var(--text-muted);font-size:12px;padding:12px;text-align:center">
            No procedures registered.
          </li>
        {:else}
          {#each procedures as proc (proc.name)}
            <li role="option" aria-selected={selected?.name === proc.name}
              onclick={() => selectProcedure(proc.name)}
              onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && selectProcedure(proc.name)}>
              <span class="proc-status-dot {proc.enabled ? 'enabled' : 'disabled'}"
                title={proc.enabled ? 'Enabled' : 'Disabled'}></span>
              <span class="proc-list-name">{proc.name}</span>
              <span class="proc-list-type">{proc.eventType}</span>
            </li>
          {/each}
        {/if}
      </ul>
    </aside>

    <!-- Right: editor + log -->
    <section class="proc-editor-panel" aria-label="Procedure editor">
      {#if !selected}
        <div class="proc-empty">
          <p>Select a procedure to view or edit it.</p>
        </div>
      {:else}
        <div class="proc-editor-view">
          <div class="proc-editor-toolbar">
            <div class="proc-meta">
              <span class="proc-detail-name">{selected.name}</span>
              <span class="proc-detail-type badge">{selected.eventType}</span>
            </div>
            <div class="proc-editor-actions">
              <label class="toggle-label" title="Enable / disable this procedure">
                <input type="checkbox" role="switch" aria-label="Enabled"
                  checked={selected.enabled} onchange={toggleEnabled} />
                <span class="toggle-track"><span class="toggle-thumb"></span></span>
                <span class="toggle-text">Enabled</span>
              </label>
              <button type="button" class="btn-secondary"
                aria-pressed={editMode}
                onclick={() => { editMode = !editMode; if (!editMode) editBody = selected.body; }}>
                {editMode ? 'Cancel' : 'Edit'}
              </button>
              {#if editMode}
                <button type="button" class="btn-primary" onclick={saveProcedure}>Save</button>
              {/if}
            </div>
          </div>

          <textarea
            class="proc-body"
            spellcheck="false"
            aria-label="Procedure definition"
            readonly={!editMode}
            bind:value={editBody}
          ></textarea>

          <details class="proc-log-section" open>
            <summary class="proc-log-summary">Execution Log</summary>
            <table class="proc-log-table" aria-label="Execution log">
              <thead>
                <tr>
                  <th scope="col">Time</th>
                  <th scope="col">Duration</th>
                  <th scope="col">Trigger</th>
                </tr>
              </thead>
              <tbody>
                {#if logEntries.length === 0}
                  <tr><td colspan="3" class="log-empty">No executions recorded yet.</td></tr>
                {:else}
                  {#each logEntries as entry (entry.firedAt + entry.procedureName)}
                    <tr>
                      <td>{new Date(entry.firedAt).toLocaleTimeString()}</td>
                      <td>{entry.durationMs} ms</td>
                      <td>{entry.triggerEvent}</td>
                    </tr>
                  {/each}
                {/if}
              </tbody>
            </table>
          </details>
        </div>
      {/if}
    </section>

  </div>
</dialog>
