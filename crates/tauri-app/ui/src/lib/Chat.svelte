<script>
  import '@plures/design-dojo/tokens.css';

  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;

  let { settingsOpen = $bindable(false), proceduresOpen = $bindable(false), agentName = 'Pares Agens' } = $props();

  /**
   * @typedef {{ role: 'user' | 'agent' | 'system', content: string, time: string, streaming?: boolean, id?: string }} ChatMessage
   */

  /** @type {ChatMessage[]} */
  let messages = $state([]);
  let inputValue = $state('');
  let busy = $state(false);
  let connectionState = $state('connected');
  let messagesEl = $state(null);

  /** Format a Date as HH:MM */
  function fmtTime(date = new Date()) {
    return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }

  /** Generate unique message ID */
  function msgId() {
    return `msg-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  }

  /** Auto-scroll to bottom when messages change */
  $effect(() => {
    if (messagesEl && messages.length > 0) {
      requestAnimationFrame(() => {
        messagesEl.scrollTop = messagesEl.scrollHeight;
      });
    }
  });

  // ── Streaming listener ────────────────────────────────────────────────
  $effect(() => {
    // Listen for streamed tokens from the model router
    const unlistenChunk = listen('model-chunk', (event) => {
      const { request_id, content, done } = event.payload;
      const idx = messages.findLastIndex(m => m.id === request_id);
      if (idx >= 0) {
        if (done) {
          messages[idx] = { ...messages[idx], streaming: false };
        } else {
          messages[idx] = { ...messages[idx], content: messages[idx].content + content };
        }
        messages = [...messages]; // trigger reactivity
      }
    });

    // Listen for complete responses (non-streaming fallback)
    const unlistenResponse = listen('model-response', (event) => {
      const { request_id, content } = event.payload;
      const idx = messages.findLastIndex(m => m.id === request_id);
      if (idx >= 0) {
        messages[idx] = { ...messages[idx], content, streaming: false };
        messages = [...messages];
      } else {
        // Response for unknown request — append
        messages = [...messages, { role: 'agent', content, time: fmtTime(), id: request_id }];
      }
      busy = false;
    });

    // Listen for errors
    const unlistenError = listen('model-error', (event) => {
      const { request_id, error } = event.payload;
      const idx = messages.findLastIndex(m => m.id === request_id);
      if (idx >= 0) {
        messages[idx] = { ...messages[idx], content: `⚠️ ${error}`, streaming: false };
        messages = [...messages];
      } else {
        messages = [...messages, { role: 'system', content: `⚠️ ${error}`, time: fmtTime() }];
      }
      busy = false;
    });

    return () => {
      unlistenChunk.then(fn => fn?.());
      unlistenResponse.then(fn => fn?.());
      unlistenError.then(fn => fn?.());
    };
  });

  async function sendMessage() {
    const content = inputValue.trim();
    if (!content || busy) return;

    const id = msgId();
    inputValue = '';
    busy = true;

    messages = [...messages, { role: 'user', content, time: fmtTime(), id }];

    // Create placeholder for streaming response.
    // responseId must match the request_id used in model-chunk / model-error events.
    const responseId = `${id}-response`;
    messages = [...messages, { role: 'agent', content: '', time: fmtTime(), id: responseId, streaming: true }];

    try {
      // Pass requestId so the backend can key model-chunk / model-error events to
      // the correct placeholder message.  The invoke blocks until the adapter
      // callback completes (i.e. until the stream is exhausted).
      //
      // Streaming path: invoke returns "" — content arrives via model-chunk events.
      // Non-streaming path (MCP tools): invoke returns the full response string.
      const response = await invoke('send_message', { content, requestId: responseId });

      // Only apply non-streaming update when the backend returned actual content
      // (i.e. the MCP tool-call path).  For the streaming path (response === "")
      // the model-chunk events have already updated the placeholder.
      if (response) {
        const idx = messages.findLastIndex(m => m.id === responseId);
        if (idx >= 0 && messages[idx].streaming) {
          messages[idx] = { ...messages[idx], content: response, streaming: false };
          messages = [...messages];
        }
      }
    } catch (err) {
      const idx = messages.findLastIndex(m => m.id === responseId);
      if (idx >= 0) {
        messages[idx] = { ...messages[idx], content: `⚠️ ${err}`, streaming: false };
        messages = [...messages];
      }
    } finally {
      busy = false;
    }
  }

  function handleKeydown(e) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
  }

  function clearChat() {
    messages = [];
  }

  /** Open settings from system-tray "Settings" menu item. */
  $effect(() => {
    const unlisten = listen('show-settings', () => { settingsOpen = true; });
    return () => { unlisten.then(fn => fn?.()); };
  });
</script>

<main class="chat-panel">
  <header class="chat-header">
    <div class="header-left">
      <span class="status-dot {connectionState}" title={connectionState}></span>
      <h1>{agentName}</h1>
    </div>
    <nav class="header-nav">
      <button class="icon-btn" title="Clear chat" onclick={clearChat}>🗑</button>
      <button class="icon-btn" title="Procedures" aria-haspopup="dialog"
        onclick={() => { proceduresOpen = true; }}>⚡</button>
      <button class="icon-btn" title="Settings" aria-haspopup="dialog"
        onclick={() => { settingsOpen = true; }}>⚙</button>
    </nav>
  </header>

  <section class="message-list" role="log" aria-live="polite" aria-label="Conversation" bind:this={messagesEl}>
    {#if messages.length === 0}
      <div class="welcome">
        <div class="welcome-icon">🤖</div>
        <h2>Hello!</h2>
        <p>I'm <strong>{agentName}</strong>. How can I help you today?</p>
        <p class="welcome-hint">Type a message below to get started.</p>
      </div>
    {/if}
    {#each messages as msg, i (msg.id ?? i)}
      <div class="message {msg.role}" class:streaming={msg.streaming}>
        <div class="message-meta">
          <span class="message-sender">{msg.role === 'user' ? 'You' : msg.role === 'system' ? 'System' : agentName}</span>
          <span class="message-time">{msg.time}</span>
        </div>
        <div class="message-bubble">
          {#if msg.streaming && !msg.content}
            <span class="typing-dots">
              <span class="dot"></span>
              <span class="dot"></span>
              <span class="dot"></span>
            </span>
          {:else}
            <div class="message-content">{@html formatContent(msg.content)}</div>
          {/if}
        </div>
      </div>
    {/each}
  </section>

  <form class="chat-form" autocomplete="off"
    onsubmit={(e) => { e.preventDefault(); sendMessage(); }}>
    <div class="input-row">
      <textarea
        class="chat-input"
        placeholder="Type a message… (Enter to send, Shift+Enter for newline)"
        rows="1"
        aria-label="Message input"
        bind:value={inputValue}
        onkeydown={handleKeydown}
        disabled={false}
      ></textarea>
      <button type="submit" class="send-btn" title="Send message" disabled={busy || !inputValue.trim()}>
        {#if busy}
          <span class="spinner"></span>
        {:else}
          <span class="send-icon">➤</span>
        {/if}
      </button>
    </div>
  </form>
</main>

<script context="module">
  /** Simple markdown-ish formatting for chat messages */
  function formatContent(text) {
    if (!text) return '';
    return text
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/\*\*(.*?)\*\*/g, '<strong>$1</strong>')
      .replace(/`([^`]+)`/g, '<code>$1</code>')
      .replace(/\n/g, '<br>');
  }
</script>

<style>
  .chat-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--surface-primary, #0a0a0f);
    color: var(--text-primary, #e8e8f0);
    font-family: var(--font-sans, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif);
  }

  /* ── Header ────────────────────────────────────────────────────────── */

  .chat-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
    background: var(--surface-elevated, rgba(255, 255, 255, 0.03));
    border-bottom: 1px solid var(--border-subtle, rgba(255, 255, 255, 0.06));
    -webkit-app-region: drag;
  }

  .header-left {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .chat-header h1 {
    font-size: 15px;
    font-weight: 600;
    margin: 0;
    letter-spacing: -0.01em;
  }

  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .status-dot.connected { background: #34d399; box-shadow: 0 0 6px rgba(52, 211, 153, 0.4); }
  .status-dot.thinking { background: #fbbf24; animation: pulse 1.5s infinite; }
  .status-dot.disconnected { background: #f87171; }

  .header-nav {
    display: flex;
    gap: 4px;
    -webkit-app-region: no-drag;
  }

  .icon-btn {
    background: transparent;
    border: 1px solid transparent;
    border-radius: 6px;
    padding: 4px 8px;
    cursor: pointer;
    font-size: 16px;
    color: var(--text-secondary, #a0a0b0);
    transition: all 0.15s;
  }
  .icon-btn:hover {
    background: var(--surface-hover, rgba(255, 255, 255, 0.06));
    border-color: var(--border-subtle, rgba(255, 255, 255, 0.08));
  }

  /* ── Messages ──────────────────────────────────────────────────────── */

  .message-list {
    flex: 1;
    overflow-y: auto;
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    scroll-behavior: smooth;
  }

  .welcome {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    flex: 1;
    text-align: center;
    opacity: 0.7;
    gap: 8px;
    padding: 40px 20px;
  }
  .welcome-icon { font-size: 48px; }
  .welcome h2 { margin: 0; font-size: 24px; font-weight: 600; }
  .welcome p { margin: 0; color: var(--text-secondary, #a0a0b0); }
  .welcome-hint { font-size: 13px; }

  .message {
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-width: 80%;
    animation: fadeIn 0.2s ease;
  }
  .message.user { align-self: flex-end; }
  .message.agent, .message.system { align-self: flex-start; }

  .message-meta {
    display: flex;
    gap: 8px;
    font-size: 11px;
    color: var(--text-tertiary, #707080);
    padding: 0 4px;
  }
  .message.user .message-meta { justify-content: flex-end; }

  .message-bubble {
    padding: 10px 14px;
    border-radius: 12px;
    font-size: 14px;
    line-height: 1.5;
    word-wrap: break-word;
  }
  .message.user .message-bubble {
    background: var(--accent-primary, #6366f1);
    color: white;
    border-bottom-right-radius: 4px;
  }
  .message.agent .message-bubble {
    background: var(--surface-elevated, rgba(255, 255, 255, 0.06));
    border: 1px solid var(--border-subtle, rgba(255, 255, 255, 0.08));
    border-bottom-left-radius: 4px;
  }
  .message.system .message-bubble {
    background: rgba(251, 191, 36, 0.1);
    border: 1px solid rgba(251, 191, 36, 0.2);
    color: #fbbf24;
    font-size: 13px;
  }

  .message-content :global(code) {
    background: rgba(0, 0, 0, 0.3);
    padding: 1px 5px;
    border-radius: 4px;
    font-family: var(--font-mono, 'JetBrains Mono', monospace);
    font-size: 13px;
  }

  /* ── Typing indicator ──────────────────────────────────────────────── */

  .typing-dots {
    display: inline-flex;
    gap: 4px;
    padding: 4px 0;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--text-tertiary, #707080);
    animation: bounce 1.2s infinite;
  }
  .dot:nth-child(2) { animation-delay: 0.2s; }
  .dot:nth-child(3) { animation-delay: 0.4s; }

  .message.streaming .message-bubble {
    border-color: var(--accent-primary, #6366f1);
  }

  /* ── Input ─────────────────────────────────────────────────────────── */

  .chat-form {
    padding: 12px 16px;
    border-top: 1px solid var(--border-subtle, rgba(255, 255, 255, 0.06));
    background: var(--surface-elevated, rgba(255, 255, 255, 0.02));
  }

  .input-row {
    display: flex;
    gap: 8px;
    align-items: flex-end;
  }

  .chat-input {
    flex: 1;
    background: var(--surface-primary, #0a0a0f);
    border: 1px solid var(--border-default, rgba(255, 255, 255, 0.1));
    border-radius: 10px;
    color: var(--text-primary, #e8e8f0);
    padding: 10px 14px;
    font-size: 14px;
    font-family: inherit;
    resize: none;
    outline: none;
    transition: border-color 0.15s;
    min-height: 20px;
    max-height: 120px;
  }
  .chat-input:focus {
    border-color: var(--accent-primary, #6366f1);
    box-shadow: 0 0 0 2px rgba(99, 102, 241, 0.15);
  }
  .chat-input::placeholder {
    color: var(--text-tertiary, #505060);
  }

  .send-btn {
    width: 40px;
    height: 40px;
    border-radius: 10px;
    background: var(--accent-primary, #6366f1);
    border: none;
    color: white;
    font-size: 18px;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: all 0.15s;
    flex-shrink: 0;
  }
  .send-btn:hover:not(:disabled) {
    background: var(--accent-hover, #818cf8);
    transform: scale(1.05);
  }
  .send-btn:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .spinner {
    width: 16px;
    height: 16px;
    border: 2px solid rgba(255, 255, 255, 0.3);
    border-top-color: white;
    border-radius: 50%;
    animation: spin 0.6s linear infinite;
  }

  /* ── Animations ────────────────────────────────────────────────────── */

  @keyframes fadeIn {
    from { opacity: 0; transform: translateY(8px); }
    to { opacity: 1; transform: translateY(0); }
  }
  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.5; }
  }
  @keyframes bounce {
    0%, 60%, 100% { transform: translateY(0); }
    30% { transform: translateY(-4px); }
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }
</style>
