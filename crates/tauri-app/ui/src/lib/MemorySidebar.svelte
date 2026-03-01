<script>
  const { invoke } = window.__TAURI__.core;

  const CATEGORY_CSS = {
    'code-pattern': 'memory-code',
    preference:     'memory-pref',
    decision:       'memory-dec',
    'error-fix':    'memory-err',
  };

  /** @type {{ id: string, content: string, category: string }[]} */
  let memories = $state([]);

  async function refreshMemories() {
    try {
      memories = await invoke('get_memories');
    } catch {
      // Memories are non-critical — swallow the error silently.
    }
  }

  $effect(() => {
    refreshMemories();
    const id = setInterval(refreshMemories, 5000);
    return () => clearInterval(id);
  });
</script>

<aside class="sidebar" aria-label="Memory sidebar">
  <header class="sidebar-header">
    <span class="sidebar-icon">🧠</span>
    <h2>Memories</h2>
  </header>
  <ul class="memory-list" aria-live="polite">
    {#if memories.length === 0}
      <li class="memory-empty">No memories yet.</li>
    {:else}
      {#each memories as m (m.id)}
        <li class="{CATEGORY_CSS[m.category] ?? ''}" title={m.content}>
          <span class="memory-category">{m.category}</span><br />
          <span class="memory-content">{m.content}</span>
        </li>
      {/each}
    {/if}
  </ul>
</aside>
