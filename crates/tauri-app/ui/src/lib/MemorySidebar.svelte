<script>
  const { invoke } = window.__TAURI__.core;

  const CATEGORY_CSS = {
    'code-pattern': 'memory-code',
    preference:     'memory-pref',
    decision:       'memory-dec',
    'error-fix':    'memory-err',
    conversation:   'memory-conv',
    correction:     'memory-corr',
  };

  /** Maximum characters shown as a content preview before truncation. */
  const MAX_CONTENT_PREVIEW_LENGTH = 120;

  const GUIDANCE_CATEGORIES = [
    { id: 'facts', name: 'Facts', icon: '📊' },
    { id: 'rules', name: 'Rules', icon: '📋' },
    { id: 'constraints', name: 'Constraints', icon: '⚠️' },
    { id: 'decisions', name: 'Decisions', icon: '✅' },
    { id: 'risks', name: 'Risks', icon: '⚠️' },
    { id: 'guidance', name: 'Guidance', icon: '💡' },
  ];

  /** @type {{ id: string, content: string, category: string, created_at: string }[]} */
  let memories = $state([]);
  
  /** @type {Record<string, Array<{ id: string, content: string, confidence: number, priority: number, source_spans: string[] }>>} */
  let guidanceData = $state({});
  
  /** @type {Array<{ id: string, event_type: string, timestamp: string, guidance_updated: number }}} */
  let analysisEvents = $state([]);
  
  let selectedGuidanceCategory = $state('facts');
  let showSourceTraces = $state(false);
  let isAnalyzing = $state(false);

  async function refreshMemories() {
    try {
      memories = await invoke('get_memories');
    } catch {
      // Memories are non-critical — swallow the error silently.
    }
  }

  async function refreshGuidance() {
    try {
      // Load guidance for all categories
      for (const category of GUIDANCE_CATEGORIES) {
        const guidance = await invoke('get_praxis_guidance', { category: category.id });
        guidanceData[category.id] = guidance;
      }
      guidanceData = { ...guidanceData }; // Trigger reactivity
    } catch (error) {
      console.warn('Failed to load Praxis guidance:', error);
    }
  }

  async function refreshAnalysisEvents() {
    try {
      analysisEvents = await invoke('get_analysis_events', { limit: 5 });
    } catch (error) {
      console.warn('Failed to load analysis events:', error);
    }
  }

  async function triggerAnalysis() {
    if (isAnalyzing) return;
    isAnalyzing = true;
    try {
      const count = await invoke('trigger_praxis_analysis');
      console.log(`Analyzed ${count} memories`);
      // Refresh guidance after analysis
      await refreshGuidance();
      await refreshAnalysisEvents();
    } catch (error) {
      console.error('Failed to trigger analysis:', error);
    } finally {
      isAnalyzing = false;
    }
  }

  async function showSourceSpans(spanIds) {
    try {
      const spans = await invoke('get_source_spans', { spanIds });
      console.log('Source spans:', spans);
      showSourceTraces = true;
      // In a real implementation, you'd show these spans in a modal or expanded view
    } catch (error) {
      console.error('Failed to load source spans:', error);
    }
  }

  $effect(() => {
    refreshMemories();
    refreshGuidance();
    refreshAnalysisEvents();
    
    const memoryInterval = setInterval(refreshMemories, 5000);
    const guidanceInterval = setInterval(refreshGuidance, 10000); // Less frequent for guidance
    const eventsInterval = setInterval(refreshAnalysisEvents, 8000);
    
    return () => {
      clearInterval(memoryInterval);
      clearInterval(guidanceInterval); 
      clearInterval(eventsInterval);
    };
  });
</script>

<aside class="sidebar" aria-label="Memory sidebar">
  <!-- Memory Section -->
  <section class="sidebar-section">
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
            <div class="memory-meta">
              <span class="memory-category">{m.category}</span>
              {#if m.created_at}
                <span class="memory-time">
                  {new Date(m.created_at).toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })}
                </span>
              {/if}
            </div>
            <span class="memory-content">
              {m.content.length > MAX_CONTENT_PREVIEW_LENGTH ? m.content.slice(0, MAX_CONTENT_PREVIEW_LENGTH) + '…' : m.content}
            </span>
          </li>
        {/each}
      {/if}
    </ul>
  </section>

  <!-- Praxis Guidance Section -->
  <section class="sidebar-section praxis-section">
    <header class="sidebar-header">
      <span class="sidebar-icon">🎯</span>
      <h2>Praxis Guidance</h2>
      <button 
        class="refresh-btn {isAnalyzing ? 'analyzing' : ''}"
        onclick={triggerAnalysis}
        disabled={isAnalyzing}
        title="Refresh guidance analysis"
      >
        {isAnalyzing ? '⏳' : '🔄'}
      </button>
    </header>

    <!-- Category Tabs -->
    <div class="guidance-tabs">
      {#each GUIDANCE_CATEGORIES as category (category.id)}
        <button 
          class="tab-btn {selectedGuidanceCategory === category.id ? 'active' : ''}"
          onclick={() => selectedGuidanceCategory = category.id}
          title="{category.name}"
        >
          <span class="tab-icon">{category.icon}</span>
          <span class="tab-label">{category.name}</span>
          {#if guidanceData[category.id]?.length}
            <span class="tab-count">{guidanceData[category.id].length}</span>
          {/if}
        </button>
      {/each}
    </div>

    <!-- Guidance Content -->
    <div class="guidance-content">
      {#if guidanceData[selectedGuidanceCategory]?.length > 0}
        <ul class="guidance-list">
          {#each guidanceData[selectedGuidanceCategory] as guidance (guidance.id)}
            <li class="guidance-item priority-{guidance.priority}">
              <div class="guidance-header">
                <span class="confidence-score" title="Confidence: {(guidance.confidence * 100).toFixed(0)}%">
                  {(guidance.confidence * 100).toFixed(0)}%
                </span>
                <span class="priority-indicator">P{guidance.priority}</span>
              </div>
              <div class="guidance-text">{guidance.content}</div>
              {#if guidance.source_spans?.length > 0}
                <button 
                  class="source-link"
                  onclick={() => showSourceSpans(guidance.source_spans)}
                  title="View source memories"
                >
                  📎 {guidance.source_spans.length} source{guidance.source_spans.length !== 1 ? 's' : ''}
                </button>
              {/if}
            </li>
          {/each}
        </ul>
      {:else}
        <div class="guidance-empty">
          <span class="empty-icon">💭</span>
          <p>No {GUIDANCE_CATEGORIES.find(c => c.id === selectedGuidanceCategory)?.name.toLowerCase()} yet</p>
          <p class="empty-hint">Chat more to generate guidance</p>
        </div>
      {/if}
    </div>
  </section>

  <!-- Analysis Activity -->
  {#if analysisEvents.length > 0}
    <section class="sidebar-section activity-section">
      <header class="sidebar-header">
        <span class="sidebar-icon">⚡</span>
        <h3>Analysis Activity</h3>
      </header>
      <ul class="activity-list">
        {#each analysisEvents as event (event.id)}
          <li class="activity-item">
            <span class="activity-time">
              {new Date(event.timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
            </span>
            <span class="activity-desc">
              {event.guidance_updated} guidance updated
            </span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}
</aside>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: 1.5rem;
    padding: 1rem;
    background: var(--color-bg-secondary, #1e1e1e);
    border-right: 1px solid var(--color-border, #333);
    height: 100vh;
    overflow-y: auto;
    width: 320px;
    font-size: 0.9rem;
  }

  .sidebar-section {
    background: var(--color-bg-primary, #2a2a2a);
    border-radius: 8px;
    overflow: hidden;
  }

  .sidebar-header {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.75rem 1rem;
    background: var(--color-bg-tertiary, #333);
    border-bottom: 1px solid var(--color-border, #444);
    font-weight: 600;
  }

  .sidebar-header h2, .sidebar-header h3 {
    margin: 0;
    font-size: 1rem;
    color: var(--color-text-primary, #fff);
  }

  .sidebar-icon {
    font-size: 1.1rem;
  }

  .refresh-btn {
    margin-left: auto;
    background: none;
    border: none;
    font-size: 1rem;
    cursor: pointer;
    padding: 0.25rem;
    border-radius: 4px;
    color: var(--color-text-secondary, #ccc);
    transition: all 0.2s;
  }

  .refresh-btn:hover {
    background: var(--color-bg-hover, #555);
    color: var(--color-text-primary, #fff);
  }

  .refresh-btn.analyzing {
    animation: pulse 1.5s infinite;
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.5; }
  }

  /* Memory List Styles */
  .memory-list {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 300px;
    overflow-y: auto;
  }

  .memory-list li {
    padding: 0.75rem 1rem;
    border-bottom: 1px solid var(--color-border, #444);
    color: var(--color-text-secondary, #ccc);
    font-size: 0.85rem;
    line-height: 1.4;
  }

  .memory-empty {
    color: var(--color-text-tertiary, #999);
    font-style: italic;
    text-align: center;
  }

  .memory-meta {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    margin-bottom: 0.25rem;
  }

  .memory-category {
    font-weight: 500;
    text-transform: uppercase;
    font-size: 0.75rem;
    opacity: 0.8;
  }

  .memory-time {
    font-size: 0.7rem;
    color: var(--color-text-tertiary, #999);
    margin-left: auto;
    padding-left: 0.5rem;
  }

  .memory-content {
    display: block;
    margin-top: 0;
  }

  .memory-code { border-left: 3px solid #4ade80; }
  .memory-pref { border-left: 3px solid #60a5fa; }
  .memory-dec  { border-left: 3px solid #fbbf24; }
  .memory-err  { border-left: 3px solid #f87171; }
  .memory-conv { border-left: 3px solid #a78bfa; }
  .memory-corr { border-left: 3px solid #fb923c; }

  /* Praxis Guidance Styles */
  .praxis-section {
    flex: 1;
  }

  .guidance-tabs {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    background: var(--color-bg-tertiary, #333);
    border-bottom: 1px solid var(--color-border, #444);
  }

  .tab-btn {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.25rem;
    padding: 0.5rem 0.25rem;
    background: none;
    border: none;
    border-right: 1px solid var(--color-border, #444);
    color: var(--color-text-secondary, #ccc);
    cursor: pointer;
    transition: all 0.2s;
    font-size: 0.75rem;
  }

  .tab-btn:nth-child(2n) {
    border-right: none;
  }

  .tab-btn:hover, .tab-btn.active {
    background: var(--color-bg-hover, #555);
    color: var(--color-text-primary, #fff);
  }

  .tab-icon {
    font-size: 1rem;
  }

  .tab-label {
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.02em;
  }

  .tab-count {
    background: var(--color-accent, #0ea5e9);
    color: white;
    border-radius: 10px;
    padding: 0.1rem 0.4rem;
    font-size: 0.7rem;
    font-weight: 600;
    min-width: 1.2rem;
    text-align: center;
  }

  .guidance-content {
    min-height: 200px;
    max-height: 400px;
    overflow-y: auto;
  }

  .guidance-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .guidance-item {
    padding: 1rem;
    border-bottom: 1px solid var(--color-border, #444);
  }

  .guidance-item.priority-1 {
    border-left: 3px solid #ef4444; /* High priority - red */
  }

  .guidance-item.priority-2 {
    border-left: 3px solid #f97316; /* Medium-high priority - orange */
  }

  .guidance-item.priority-3 {
    border-left: 3px solid #eab308; /* Medium priority - yellow */
  }

  .guidance-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 0.5rem;
  }

  .confidence-score {
    background: var(--color-bg-tertiary, #333);
    color: var(--color-text-primary, #fff);
    padding: 0.15rem 0.4rem;
    border-radius: 4px;
    font-size: 0.75rem;
    font-weight: 500;
  }

  .priority-indicator {
    background: var(--color-accent, #0ea5e9);
    color: white;
    padding: 0.15rem 0.4rem;
    border-radius: 4px;
    font-size: 0.75rem;
    font-weight: 500;
  }

  .guidance-text {
    color: var(--color-text-primary, #fff);
    line-height: 1.4;
    margin-bottom: 0.5rem;
  }

  .source-link {
    background: none;
    border: 1px solid var(--color-border, #444);
    color: var(--color-text-secondary, #ccc);
    padding: 0.25rem 0.5rem;
    border-radius: 4px;
    font-size: 0.75rem;
    cursor: pointer;
    transition: all 0.2s;
  }

  .source-link:hover {
    background: var(--color-bg-hover, #555);
    color: var(--color-text-primary, #fff);
  }

  .guidance-empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 2rem 1rem;
    color: var(--color-text-tertiary, #999);
    text-align: center;
  }

  .empty-icon {
    font-size: 2rem;
    margin-bottom: 0.5rem;
  }

  .empty-hint {
    font-size: 0.8rem;
    opacity: 0.8;
    margin-top: 0.25rem;
  }

  /* Analysis Activity Styles */
  .activity-section {
    flex-shrink: 0;
  }

  .activity-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .activity-item {
    display: flex;
    justify-content: space-between;
    padding: 0.5rem 1rem;
    border-bottom: 1px solid var(--color-border, #444);
    font-size: 0.8rem;
  }

  .activity-time {
    color: var(--color-text-tertiary, #999);
  }

  .activity-desc {
    color: var(--color-text-secondary, #ccc);
  }
</style>