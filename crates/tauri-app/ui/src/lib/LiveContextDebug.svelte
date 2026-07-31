<script>
  import { onMount } from 'svelte';
  import ChronicleViewer from '@plures/design-dojo/app/ChronicleViewer.svelte';

  const tauriEvent = typeof window !== 'undefined' ? window.__TAURI__?.event : undefined;
  const invoke = typeof window !== 'undefined' ? window.__TAURI__?.core?.invoke : undefined;

  let nodes = $state([]);
  let pausedSessions = $state(new Set());
  let selected = $state(null);
  let nextLocalId = 0;

  function selectedSessionId() {
    if (!selected?.context) return null;
    try { return JSON.parse(selected.context).sessionId ?? null; } catch { return null; }
  }

  function valueSummary(value) {
    if (typeof value === 'string') return value;
    try { return JSON.stringify(value); } catch { return String(value); }
  }

  function addLiveContext(payload) {
    const event = payload?.event;
    if (!payload?.session_id || !event) return;

    const id = event.id ?? `${payload.session_id}:${event.timestamp ?? Date.now()}:${nextLocalId++}`;
    if (nodes.some((node) => node.id === id)) return;

    nodes = [...nodes, {
      id,
      timestamp: event.timestamp ?? Date.now(),
      path: event.path ?? `sessions/${payload.session_id}/live-context`,
      diff: {
        before: event.before ?? null,
        after: event.after ?? event
      },
      cause: event.cause ?? null,
      context: JSON.stringify({ sessionId: payload.session_id, summary: event.summary ?? valueSummary(event) }),
      operations: event.operations ?? []
    }].slice(-256);
  }

  async function togglePause() {
    if (!selected || !invoke) return;
    const sessionId = selectedSessionId();
    if (!sessionId) return;
    const paused = !pausedSessions.has(sessionId);
    await invoke('set_live_context_paused', { sessionId, paused });
    const next = new Set(pausedSessions);
    if (paused) next.add(sessionId); else next.delete(sessionId);
    pausedSessions = next;
  }

  onMount(() => {
    if (!tauriEvent?.listen) return;
    const unlisten = tauriEvent.listen('chronos-live-context', ({ payload }) => addLiveContext(payload));
    return () => { unlisten.then((fn) => fn?.()); };
  });
</script>

<section class="live-context-debug" aria-label="Chronos live context debug viewer">
  <header>
    <div>
      <h2>Live context</h2>
      <p>Chronos events from the active render session. Select an operation card to inspect its real child operations.</p>
    </div>
    {#if selected}
      <button type="button" onclick={togglePause}>
        {pausedSessions.has(selectedSessionId()) ? 'Resume live context' : 'Pause live context'}
      </button>
    {/if}
  </header>
  <ChronicleViewer {nodes} onnodeselect={(node) => selected = node} />
</section>

<style>
  .live-context-debug { border-top: 1px solid var(--color-border, #30363d); padding: 1rem; min-block-size: 18rem; }
  header { display: flex; align-items: start; justify-content: space-between; gap: 1rem; }
  h2 { margin: 0; font-size: 1rem; }
  p { margin: .25rem 0 1rem; color: var(--color-text-muted, #8b949e); }
  button { border: 1px solid var(--color-border, #57606a); border-radius: .375rem; background: transparent; color: inherit; cursor: pointer; padding: .4rem .7rem; }
</style>
