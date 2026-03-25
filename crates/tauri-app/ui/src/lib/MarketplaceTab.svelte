<script>
  const { invoke } = window.__TAURI__.core;

  /**
   * @typedef {{ id: string, name: string, version: string, description: string, author: string, mean_score: number, review_count: number }} CatalogEntry
   * @typedef {{ id: string, name: string, version: string, description: string, author: string, installed_at: string, last_used: string|null }} InstalledEntry
   * @typedef {{ skill_id: string, installed_version: string, available_version: string }} UpdateEntry
   */

  /** @type {CatalogEntry[]} */
  let searchResults = $state([]);
  /** @type {InstalledEntry[]} */
  let installed = $state([]);
  /** @type {UpdateEntry[]} */
  let availableUpdates = $state([]);

  let searchQuery = $state('');
  let searching = $state(false);
  let searchError = $state('');

  let installing = $state(/** @type {string|null} */ (null));
  let removing = $state(/** @type {string|null} */ (null));
  let updatingAll = $state(false);

  /** @type {'search'|'installed'} */
  let activeSection = $state('search');

  $effect(() => {
    refreshInstalled();
  });

  async function refreshInstalled() {
    try {
      installed = await invoke('marketplace_list_installed');
    } catch {
      installed = [];
    }
    try {
      availableUpdates = await invoke('marketplace_check_updates');
    } catch {
      availableUpdates = [];
    }
  }

  async function search() {
    const q = searchQuery.trim();
    if (!q) return;
    searching = true;
    searchError = '';
    try {
      searchResults = await invoke('marketplace_search', { query: q });
    } catch (err) {
      searchError = String(err);
      searchResults = [];
    } finally {
      searching = false;
    }
  }

  async function install(id) {
    installing = id;
    try {
      await invoke('marketplace_install', { id });
      await refreshInstalled();
    } catch (err) {
      searchError = `Install failed: ${err}`;
    } finally {
      installing = null;
    }
  }

  async function remove(id) {
    removing = id;
    try {
      await invoke('marketplace_remove', { id });
      await refreshInstalled();
    } catch (err) {
      searchError = `Remove failed: ${err}`;
    } finally {
      removing = null;
    }
  }

  async function updateAll() {
    updatingAll = true;
    try {
      await invoke('marketplace_update_all');
      await refreshInstalled();
    } catch (err) {
      searchError = `Update failed: ${err}`;
    } finally {
      updatingAll = false;
    }
  }

  /** @param {string} id */
  function isInstalled(id) {
    return installed.some(s => s.id === id);
  }

  /** @param {number} score */
  function formatScore(score) {
    return score > 0 ? `★ ${score.toFixed(1)}` : '—';
  }
</script>

<div class="marketplace-root">
  <!-- Section toggle -->
  <div class="mkt-sections" role="tablist" aria-label="Marketplace sections">
    <button
      type="button"
      role="tab"
      aria-selected={activeSection === 'search'}
      tabindex={activeSection === 'search' ? 0 : -1}
      class="mkt-section-btn"
      class:active={activeSection === 'search'}
      onclick={() => { activeSection = 'search'; }}>
      Discover
    </button>
    <button
      type="button"
      role="tab"
      aria-selected={activeSection === 'installed'}
      tabindex={activeSection === 'installed' ? 0 : -1}
      class="mkt-section-btn"
      class:active={activeSection === 'installed'}
      onclick={() => { activeSection = 'installed'; }}>
      Installed
      {#if installed.length > 0}
        <span class="mkt-badge">{installed.length}</span>
      {/if}
    </button>
  </div>

  <!-- Discover panel -->
  {#if activeSection === 'search'}
    <div role="tabpanel" class="mkt-panel">
      <div class="mkt-search-row">
        <input
          type="search"
          class="mkt-search-input"
          bind:value={searchQuery}
          placeholder="Search procedures…"
          aria-label="Search marketplace procedures"
          onkeydown={(e) => { if (e.key === 'Enter') search(); }}
        />
        <button
          type="button"
          class="btn-primary mkt-search-btn"
          onclick={search}
          disabled={searching || !searchQuery.trim()}
          aria-busy={searching}>
          {searching ? 'Searching…' : 'Search'}
        </button>
      </div>

      {#if searchError}
        <p class="mkt-error" role="alert">{searchError}</p>
      {/if}

      {#if searchResults.length === 0 && !searching}
        <p class="pref-hint">Enter a query above to discover community procedures.</p>
      {:else}
        <ul class="mkt-results" aria-label="Search results">
          {#each searchResults as entry (entry.id)}
            <li class="mkt-card">
              <div class="mkt-card-header">
                <span class="mkt-card-name">{entry.name}</span>
                <span class="mkt-card-version">v{entry.version}</span>
                <span class="mkt-card-score" aria-label="Rating">{formatScore(entry.mean_score)}</span>
              </div>
              <p class="mkt-card-desc">{entry.description}</p>
              <div class="mkt-card-footer">
                <span class="mkt-card-author">by {entry.author}</span>
                {#if entry.review_count > 0}
                  <span class="mkt-card-reviews">{entry.review_count} review{entry.review_count === 1 ? '' : 's'}</span>
                {/if}
                {#if isInstalled(entry.id)}
                  <button
                    type="button"
                    class="mkt-btn-remove"
                    onclick={() => remove(entry.id)}
                    disabled={removing === entry.id}
                    aria-label="Remove {entry.name}">
                    {removing === entry.id ? 'Removing…' : 'Remove'}
                  </button>
                {:else}
                  <button
                    type="button"
                    class="btn-primary mkt-btn-install"
                    onclick={() => install(entry.id)}
                    disabled={installing === entry.id}
                    aria-label="Install {entry.name}">
                    {installing === entry.id ? 'Installing…' : 'Install'}
                  </button>
                {/if}
              </div>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}

  <!-- Installed panel -->
  {#if activeSection === 'installed'}
    <div role="tabpanel" class="mkt-panel">
      {#if availableUpdates.length > 0}
        <div class="mkt-update-banner" role="status">
          <span>{availableUpdates.length} update{availableUpdates.length === 1 ? '' : 's'} available</span>
          <button
            type="button"
            class="btn-primary"
            onclick={updateAll}
            disabled={updatingAll}
            aria-busy={updatingAll}>
            {updatingAll ? 'Updating…' : 'Update All'}
          </button>
        </div>
      {/if}

      {#if installed.length === 0}
        <p class="pref-hint">No procedures installed yet. Use the Discover tab to find and install procedures.</p>
      {:else}
        <ul class="mkt-installed-list" aria-label="Installed procedures">
          {#each installed as skill (skill.id)}
            {@const hasUpdate = availableUpdates.some(u => u.skill_id === skill.id)}
            <li class="mkt-card" class:mkt-has-update={hasUpdate}>
              <div class="mkt-card-header">
                <span class="mkt-card-name">{skill.name}</span>
                <span class="mkt-card-version">v{skill.version}</span>
                {#if hasUpdate}
                  <span class="mkt-update-dot" aria-label="Update available">●</span>
                {/if}
              </div>
              <p class="mkt-card-desc">{skill.description}</p>
              <div class="mkt-card-footer">
                <span class="mkt-card-meta">
                  Installed {skill.installed_at}
                  {#if skill.last_used}
                    · Last used {skill.last_used}
                  {:else}
                    · Never used
                  {/if}
                </span>
                <button
                  type="button"
                  class="mkt-btn-remove"
                  onclick={() => remove(skill.id)}
                  disabled={removing === skill.id}
                  aria-label="Remove {skill.name}">
                  {removing === skill.id ? 'Removing…' : 'Remove'}
                </button>
              </div>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
</div>

<style>
  .marketplace-root {
    display: flex;
    flex-direction: column;
    gap: var(--space-3, 12px);
  }

  /* Section toggles */
  .mkt-sections {
    display: flex;
    gap: 0;
    border-bottom: 1px solid var(--color-border, #3a3a4a);
  }

  .mkt-section-btn {
    padding: 6px 14px;
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    color: var(--color-muted, #888);
    cursor: pointer;
    font-size: 0.875rem;
    display: flex;
    align-items: center;
    gap: 6px;
    transition: color 0.15s, border-color 0.15s;
  }

  .mkt-section-btn.active,
  .mkt-section-btn:hover {
    color: var(--color-text, #e0e0e0);
    border-bottom-color: var(--color-accent, #7c6af7);
  }

  .mkt-badge {
    background: var(--color-accent, #7c6af7);
    color: #fff;
    font-size: 0.7rem;
    border-radius: 999px;
    padding: 1px 6px;
    line-height: 1.4;
  }

  /* Panel */
  .mkt-panel {
    display: flex;
    flex-direction: column;
    gap: var(--space-3, 12px);
  }

  /* Search row */
  .mkt-search-row {
    display: flex;
    gap: 8px;
  }

  .mkt-search-input {
    flex: 1;
    padding: 6px 10px;
    border-radius: var(--radius-sm, 4px);
    border: 1px solid var(--color-border, #3a3a4a);
    background: var(--color-surface, #1e1e2e);
    color: var(--color-text, #e0e0e0);
    font-size: 0.875rem;
  }

  .mkt-search-btn {
    white-space: nowrap;
  }

  /* Cards */
  .mkt-results,
  .mkt-installed-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .mkt-card {
    padding: 10px 12px;
    border-radius: var(--radius-sm, 4px);
    border: 1px solid var(--color-border, #3a3a4a);
    background: var(--color-surface, #1e1e2e);
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .mkt-has-update {
    border-color: var(--color-accent, #7c6af7);
  }

  .mkt-card-header {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }

  .mkt-card-name {
    font-weight: 600;
    font-size: 0.9rem;
    color: var(--color-text, #e0e0e0);
  }

  .mkt-card-version {
    font-size: 0.78rem;
    color: var(--color-muted, #888);
  }

  .mkt-card-score {
    margin-left: auto;
    font-size: 0.8rem;
    color: var(--color-accent, #7c6af7);
  }

  .mkt-update-dot {
    margin-left: auto;
    color: var(--color-accent, #7c6af7);
    font-size: 0.7rem;
  }

  .mkt-card-desc {
    font-size: 0.82rem;
    color: var(--color-muted, #aaa);
    margin: 0;
  }

  .mkt-card-footer {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    margin-top: 4px;
  }

  .mkt-card-author,
  .mkt-card-reviews,
  .mkt-card-meta {
    font-size: 0.78rem;
    color: var(--color-muted, #888);
    flex: 1;
  }

  .mkt-btn-install,
  .mkt-btn-remove {
    padding: 3px 10px;
    font-size: 0.8rem;
    border-radius: var(--radius-sm, 4px);
    border: 1px solid var(--color-border, #3a3a4a);
    cursor: pointer;
  }

  .mkt-btn-remove {
    background: transparent;
    color: var(--color-danger, #e06c75);
    border-color: var(--color-danger, #e06c75);
  }

  .mkt-btn-remove:hover:not(:disabled) {
    background: var(--color-danger, #e06c75);
    color: #fff;
  }

  .mkt-btn-remove:disabled,
  .mkt-btn-install:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  /* Update banner */
  .mkt-update-banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 12px;
    border-radius: var(--radius-sm, 4px);
    background: color-mix(in srgb, var(--color-accent, #7c6af7) 12%, transparent);
    border: 1px solid var(--color-accent, #7c6af7);
    font-size: 0.875rem;
    color: var(--color-text, #e0e0e0);
  }

  /* Error */
  .mkt-error {
    color: var(--color-danger, #e06c75);
    font-size: 0.82rem;
    margin: 0;
  }

  /* Hint */
  .pref-hint {
    font-size: 0.82rem;
    color: var(--color-muted, #888);
    margin: 0;
  }
</style>
