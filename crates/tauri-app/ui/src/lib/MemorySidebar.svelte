<script>
  import MemorySidebarDD from '@plures/design-dojo/app/MemorySidebar.svelte';
  import SemanticSearchInput from '@plures/design-dojo/app/SemanticSearchInput.svelte';

  const { invoke } = window.__TAURI__.core;

  /** @type {import('@plures/design-dojo/app/MemorySidebar.types.js').Memory[]} */
  let memories = $state([]);
  let searchQuery = $state('');
  let filteredMemories = $derived(
    searchQuery.trim()
      ? memories.filter(m =>
          m.content.toLowerCase().includes(searchQuery.toLowerCase()) ||
          m.category.toLowerCase().includes(searchQuery.toLowerCase()) ||
          (m.tags ?? []).some(t => t.toLowerCase().includes(searchQuery.toLowerCase()))
        )
      : memories
  );

  // ── Guidance state (Praxis coprocessor) ──────────────────────────────────
  const GUIDANCE_CATEGORIES = [
    { id: 'facts', name: 'Facts', icon: '📊' },
    { id: 'rules', name: 'Rules', icon: '📋' },
    { id: 'constraints', name: 'Constraints', icon: '⚠️' },
  ];

  /** @type {Record<string, Array<{ id: string, content: string, confidence: number, priority: number }>>} */
  let guidanceData = $state({});
  let selectedGuidanceCategory = $state('facts');
  let isAnalyzing = $state(false);

  async function refreshMemories() {
    try {
      const raw = await invoke('get_memories');
      memories = raw.map((/** @type {any} */ m) => ({
        id: m.id,
        content: m.content,
        category: mapCategory(m.category),
        timestamp: new Date(m.timestamp ?? Date.now()),
        tags: m.tags ?? [],
      }));
    } catch {
      // non-critical
    }
  }

  /** @param {string} cat */
  function mapCategory(cat) {
    const map = { 'code-pattern': 'context', 'error-fix': 'context', preference: 'preference', decision: 'goal' };
    return map[cat] ?? 'other';
  }

  async function refreshGuidance() {
    try {
      for (const category of GUIDANCE_CATEGORIES) {
        const guidance = await invoke('get_praxis_guidance', { category: category.id });
        guidanceData[category.id] = guidance;
      }
      guidanceData = { ...guidanceData };
    } catch (error) {
      console.warn('Failed to load Praxis guidance:', error);
    }
  }

  async function triggerAnalysis() {
    isAnalyzing = true;
    try {
      await invoke('trigger_praxis_analysis');
      await refreshGuidance();
    } catch (error) {
      console.warn('Analysis failed:', error);
    } finally {
      isAnalyzing = false;
    }
  }

  /** @param {CustomEvent<string>} e */
  function handleSearch(e) {
    searchQuery = e.detail ?? '';
  }

  $effect(() => {
    refreshMemories();
    refreshGuidance();
    const interval = setInterval(() => { refreshMemories(); refreshGuidance(); }, 10_000);
    return () => clearInterval(interval);
  });

  let currentGuidance = $derived(guidanceData[selectedGuidanceCategory] ?? []);
</script>

<aside class="memory-sidebar">
  <div class="sidebar-header">
    <h2 class="sidebar-title">Memory</h2>
    <button class="btn-icon-sm" onclick={triggerAnalysis} disabled={isAnalyzing} title="Analyze">
      {isAnalyzing ? '⏳' : '🔍'}
    </button>
  </div>

  <div class="sidebar-search">
    <SemanticSearchInput
      placeholder="Search memories…"
      onquery={async (query) => {
        searchQuery = query;
        return filteredMemories.map(m => ({ id: m.id, content: m.content, score: 1 }));
      }}
      onselect={(result) => { searchQuery = result.content; }}
    />
  </div>

  <div class="sidebar-memories">
    <MemorySidebarDD memories={filteredMemories} />
  </div>

  {#if Object.keys(guidanceData).length > 0}
    <div class="sidebar-guidance">
      <div class="guidance-tabs">
        {#each GUIDANCE_CATEGORIES as cat}
          <button
            class="guidance-tab"
            class:active={selectedGuidanceCategory === cat.id}
            onclick={() => { selectedGuidanceCategory = cat.id; }}>
            {cat.icon} {cat.name}
            {#if (guidanceData[cat.id] ?? []).length > 0}
              <span class="guidance-count">{guidanceData[cat.id].length}</span>
            {/if}
          </button>
        {/each}
      </div>
      <div class="guidance-list">
        {#each currentGuidance as entry}
          <div class="guidance-item">
            <p class="guidance-content">{entry.content}</p>
            <span class="guidance-confidence">conf: {Math.round(entry.confidence * 100)}%</span>
          </div>
        {:else}
          <p class="guidance-empty">No {selectedGuidanceCategory} yet</p>
        {/each}
      </div>
    </div>
  {/if}
</aside>

<style>
  .memory-sidebar {
    display: flex;
    flex-direction: column;
    height: 100%;
    gap: 8px;
    padding: 12px;
    overflow-y: auto;
  }
  .sidebar-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .sidebar-title {
    font-size: 14px;
    font-weight: 600;
    color: var(--text-primary);
    margin: 0;
  }
  .btn-icon-sm {
    background: none;
    border: none;
    cursor: pointer;
    font-size: 16px;
    padding: 2px;
  }
  .sidebar-search { margin-bottom: 4px; }
  .sidebar-memories { flex: 1; min-height: 0; overflow-y: auto; }
  .sidebar-guidance {
    border-top: 1px solid var(--border-primary, rgba(255,255,255,0.08));
    padding-top: 8px;
  }
  .guidance-tabs {
    display: flex;
    gap: 4px;
    margin-bottom: 8px;
  }
  .guidance-tab {
    background: none;
    border: 1px solid transparent;
    border-radius: 4px;
    padding: 2px 6px;
    font-size: 11px;
    cursor: pointer;
    color: var(--text-secondary);
  }
  .guidance-tab.active {
    border-color: var(--border-primary, rgba(255,255,255,0.12));
    color: var(--text-primary);
    background: var(--bg-secondary, rgba(255,255,255,0.04));
  }
  .guidance-count {
    background: var(--accent-primary, #60a5fa);
    color: #000;
    border-radius: 8px;
    padding: 0 4px;
    font-size: 10px;
    margin-left: 2px;
  }
  .guidance-list { display: flex; flex-direction: column; gap: 4px; }
  .guidance-item {
    padding: 6px 8px;
    border-radius: 4px;
    background: var(--bg-secondary, rgba(255,255,255,0.03));
    font-size: 12px;
  }
  .guidance-content { margin: 0; color: var(--text-primary); }
  .guidance-confidence { font-size: 10px; color: var(--text-tertiary, rgba(255,255,255,0.4)); }
  .guidance-empty { color: var(--text-secondary); font-size: 12px; text-align: center; margin: 8px 0; }
</style>
