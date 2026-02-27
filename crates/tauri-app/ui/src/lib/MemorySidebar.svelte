<script>
  /**
   * MemorySidebar — shows recalled memories for the current conversation.
   *
   * Props:
   *   memories  {Array<{id, content, category, created_at}>}
   */
  const { memories = [] } = $props();

  const CATEGORY_CSS = {
    'code-pattern': 'memory-code',
    preference: 'memory-pref',
    decision: 'memory-dec',
    'error-fix': 'memory-err',
  };
</script>

<aside class="sidebar" aria-label="Memory sidebar">
  <header class="sidebar-header">
    <span class="sidebar-icon" aria-hidden="true">🧠</span>
    <h2>Memories</h2>
  </header>
  <ul class="memory-list" aria-live="polite" aria-label="Recalled memories">
    {#if memories.length === 0}
      <li class="memory-empty">No memories yet.</li>
    {:else}
      {#each memories as m (m.id)}
        <li class={CATEGORY_CSS[m.category] ?? ''} title={m.content}>
          <span class="memory-category">{m.category}</span><br />
          <span class="memory-content">{m.content}</span>
        </li>
      {/each}
    {/if}
  </ul>
</aside>

<style>
  .sidebar {
    background: var(--bg-surface);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    width: 240px;
    flex-shrink: 0;
  }

  .sidebar-header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 16px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }

  .sidebar-icon { font-size: 18px; }

  .sidebar-header h2 {
    font-size: 13px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-secondary);
    margin: 0;
  }

  .memory-list {
    list-style: none;
    overflow-y: auto;
    flex: 1;
    padding: 8px;
    margin: 0;
  }

  .memory-list li {
    padding: 8px 10px;
    border-radius: var(--radius-sm);
    font-size: 12px;
    color: var(--text-secondary);
    border-left: 2px solid transparent;
    margin-bottom: 4px;
    transition: background var(--transition);
    cursor: default;
  }

  .memory-list li:hover { background: var(--bg-hover); }

  .memory-list :global(.memory-code)  { border-left-color: #7c9af7; }
  .memory-list :global(.memory-pref)  { border-left-color: #f7a27c; }
  .memory-list :global(.memory-dec)   { border-left-color: #7cf7b4; }
  .memory-list :global(.memory-err)   { border-left-color: var(--danger); }
  .memory-empty { color: var(--text-muted); text-align: center; margin-top: 16px; }

  .memory-category {
    display: inline-block;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-muted);
    margin-bottom: 2px;
  }

  .memory-content {
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
</style>
