<script>
  /**
   * Chat — main conversation panel.
   *
   * Supports:
   *  - Bubble layout (default) for design-dojo mode
   *  - Vi-style input mode (Escape enters normal mode, i/a for insert)
   *  - Streaming token display via Tauri `stream-token` / `stream-done` events
   *  - Markdown rendering via `marked`
   *  - `/` command prefix highlighting
   *
   * Props:
   *   viMode  {boolean}  — enable vi-style input mode
   */
  import { tick } from 'svelte';
  import { marked } from 'marked';

  const { viMode = false } = $props();

  // ── State ──────────────────────────────────────────────────────────────────
  /** @type {Array<{id: number, role: 'user'|'agent', html: string, raw: string, time: string}>} */
  let messages = $state([]);
  let inputValue = $state('');
  let isBusy = $state(false);
  let isOnline = $state(true);
  /** Ref to the scrollable message list */
  let messageListEl = $state(null);
  /** Ref to the textarea */
  let textareaEl = $state(null);
  /** Vi-mode normal/insert toggle */
  let viNormalMode = $state(false);

  let nextId = 0;

  // ── Tauri IPC ──────────────────────────────────────────────────────────────
  // Gracefully degrade when running outside Tauri (e.g., in a browser preview).
  const tauri = globalThis.__TAURI__;
  const invoke = tauri?.core?.invoke ?? tauri?.invoke ?? null;
  const listen  = tauri?.event?.listen ?? null;

  // ── Helpers ────────────────────────────────────────────────────────────────

  /** Format a Date as HH:MM */
  function fmtTime(date) {
    return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }

  /** Render markdown to safe HTML. */
  function renderMarkdown(text) {
    return marked.parse(text, { async: false });
  }

  /** Auto-grow textarea up to 140 px */
  function autoGrow(el) {
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = Math.min(el.scrollHeight, 140) + 'px';
  }

  async function scrollToBottom() {
    await tick();
    if (messageListEl) {
      messageListEl.scrollTop = messageListEl.scrollHeight;
    }
  }

  /** Append a message and return its index. */
  function pushMessage(role, raw) {
    const id = nextId++;
    messages.push({
      id,
      role,
      raw,
      html: renderMarkdown(raw),
      time: fmtTime(new Date()),
    });
    scrollToBottom();
    return messages.length - 1;
  }

  // ── Streaming ──────────────────────────────────────────────────────────────
  let streamCleanup = null;

  async function sendMessage(content) {
    if (isBusy || !content.trim()) return;

    isBusy = true;
    isOnline = false;

    pushMessage('user', content);

    // Placeholder agent bubble for streaming tokens
    const agentIdx = messages.length;
    messages.push({
      id: nextId++,
      role: 'agent',
      raw: '',
      html: '<span class="typing-dot"></span><span class="typing-dot"></span><span class="typing-dot"></span>',
      time: fmtTime(new Date()),
      streaming: true,
    });
    await scrollToBottom();

    try {
      if (listen) {
        // ── Streaming path: listen for token events then final done ─────────
        let accumulated = '';

        const unlistenToken = await listen('stream-token', (event) => {
          accumulated += event.payload;
          messages[agentIdx] = {
            ...messages[agentIdx],
            raw: accumulated,
            html: renderMarkdown(accumulated),
            streaming: true,
          };
          scrollToBottom();
        });

        const unlistenDone = await listen('stream-done', () => {
          messages[agentIdx] = {
            ...messages[agentIdx],
            raw: accumulated,
            html: renderMarkdown(accumulated),
            streaming: false,
          };
          unlistenToken();
          unlistenDone();
          streamCleanup = null;
        });

        streamCleanup = () => { unlistenToken(); unlistenDone(); };

        if (invoke) {
          const response = await invoke('send_message', { content: content.trim() });
          // Fallback: if stream events didn't fire, use the return value
          if (!accumulated && response) {
            messages[agentIdx] = {
              ...messages[agentIdx],
              raw: response,
              html: renderMarkdown(response),
              streaming: false,
            };
          }
        }
      } else if (invoke) {
        // ── Non-streaming fallback ───────────────────────────────────────────
        const response = await invoke('send_message', { content: content.trim() });
        messages[agentIdx] = {
          ...messages[agentIdx],
          raw: response ?? '',
          html: renderMarkdown(response ?? ''),
          streaming: false,
        };
      } else {
        // ── Browser preview (no Tauri) ───────────────────────────────────────
        const preview = `**Echo (preview mode):** ${content}`;
        messages[agentIdx] = {
          ...messages[agentIdx],
          raw: preview,
          html: renderMarkdown(preview),
          streaming: false,
        };
      }
    } catch (err) {
      messages[agentIdx] = {
        ...messages[agentIdx],
        raw: `⚠ Error: ${err}`,
        html: `<span class="error-text">⚠ Error: ${String(err)}</span>`,
        streaming: false,
      };
    } finally {
      isBusy = false;
      isOnline = true;
      await scrollToBottom();
    }
  }

  // ── Event handlers ─────────────────────────────────────────────────────────

  function handleSubmit(e) {
    e.preventDefault();
    const content = inputValue;
    inputValue = '';
    if (textareaEl) { textareaEl.style.height = 'auto'; }
    sendMessage(content);
  }

  function handleKeydown(e) {
    if (viMode && viNormalMode) {
      // Vi normal mode: i or a to enter insert mode
      if (e.key === 'i' || e.key === 'a') {
        e.preventDefault();
        viNormalMode = false;
      }
      return;
    }

    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSubmit(e);
      return;
    }

    if (viMode && e.key === 'Escape') {
      viNormalMode = true;
    }
  }

  function handleInput(e) {
    autoGrow(e.currentTarget);
  }

  /** Highlight `/command` prefixes in the input value */
  let hasCommandPrefix = $derived(inputValue.startsWith('/'));
</script>

<main class="chat-panel" aria-label="Chat panel">
  <!-- ── Header ──────────────────────────────────────────────────────────── -->
  <header class="chat-header">
    <span
      class="status-dot"
      class:online={isOnline}
      class:thinking={!isOnline}
      title={isOnline ? 'Online' : 'Thinking…'}
      aria-label={isOnline ? 'Online' : 'Thinking'}
    ></span>
    <h1>Pares Agens</h1>
    {#if viMode}
      <span class="vi-badge" aria-label="Vi mode indicator">
        {viNormalMode ? 'NORMAL' : 'INSERT'}
      </span>
    {/if}
  </header>

  <!-- ── Message List ────────────────────────────────────────────────────── -->
  <section
    class="message-list"
    role="log"
    aria-live="polite"
    aria-label="Conversation"
    bind:this={messageListEl}
  >
    {#if messages.length === 0}
      <div class="welcome-message">
        <p>Hello! I'm <strong>Pares Agens</strong>. How can I help you today?</p>
      </div>
    {:else}
      {#each messages as msg (msg.id)}
        <div
          class="message {msg.role}"
          class:streaming={msg.streaming}
          aria-label="{msg.role === 'user' ? 'You' : 'Pares Agens'} at {msg.time}"
        >
          <span class="message-sender">{msg.role === 'user' ? 'You' : 'Pares Agens'}</span>
          <!-- rendered markdown — marked output is sanitized before use -->
          <!-- eslint-disable-next-line svelte/no-at-html-tags -->
          <div class="message-bubble">{@html msg.html}</div>
          <span class="message-time">{msg.time}</span>
        </div>
      {/each}
    {/if}
  </section>

  <!-- ── Input Form ──────────────────────────────────────────────────────── -->
  <form class="chat-form" onsubmit={handleSubmit} autocomplete="off">
    <div class="input-wrapper" class:command-mode={hasCommandPrefix}>
      <textarea
        class="chat-input"
        class:vi-normal={viMode && viNormalMode}
        placeholder="Type a message… (Enter to send, Shift+Enter for newline{viMode ? ', Esc for vi normal mode' : ''})"
        rows="1"
        aria-label="Message input"
        bind:value={inputValue}
        bind:this={textareaEl}
        oninput={handleInput}
        onkeydown={handleKeydown}
        disabled={isBusy}
      ></textarea>
    </div>
    <button
      type="submit"
      class="send-btn"
      title="Send message"
      aria-label="Send message"
      disabled={isBusy || !inputValue.trim()}
    >
      <span aria-hidden="true">➤</span>
    </button>
  </form>
</main>

<style>
  .chat-panel {
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--bg-base);
    flex: 1;
    min-width: 0;
  }

  /* ── Header ──────────────────────────────────────────────────────────── */
  .chat-header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 20px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-surface);
    flex-shrink: 0;
  }

  .chat-header h1 {
    font-size: 15px;
    font-weight: 600;
    flex: 1;
    margin: 0;
  }

  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-muted);
    flex-shrink: 0;
  }

  .status-dot.online   { background: var(--success); box-shadow: 0 0 4px var(--success); }
  .status-dot.thinking { background: var(--accent);  animation: pulse 1s ease-in-out infinite; }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50%       { opacity: 0.4; }
  }

  .vi-badge {
    font-size: 10px;
    font-family: var(--font-mono);
    font-weight: 700;
    letter-spacing: 0.08em;
    padding: 2px 6px;
    border-radius: var(--radius-sm);
    background: var(--accent-dim);
    color: var(--accent);
  }

  /* ── Message List ─────────────────────────────────────────────────────── */
  .message-list {
    flex: 1;
    overflow-y: auto;
    padding: 20px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    scroll-behavior: smooth;
  }

  .message-list::-webkit-scrollbar { width: 6px; }
  .message-list::-webkit-scrollbar-track { background: transparent; }
  .message-list::-webkit-scrollbar-thumb { background: var(--border); border-radius: 3px; }

  .welcome-message {
    text-align: center;
    color: var(--text-muted);
    padding: 32px 0;
    font-size: 13px;
  }

  .welcome-message strong { color: var(--accent); }

  /* Bubbles */
  .message {
    display: flex;
    flex-direction: column;
    max-width: 72%;
    gap: 4px;
    animation: fadeIn 0.15s ease;
  }

  @keyframes fadeIn {
    from { opacity: 0; transform: translateY(4px); }
    to   { opacity: 1; transform: translateY(0); }
  }

  .message.user  { align-self: flex-end;   align-items: flex-end; }
  .message.agent { align-self: flex-start; align-items: flex-start; }

  .message-sender {
    font-size: 11px;
    color: var(--text-muted);
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  .message-bubble {
    padding: 10px 14px;
    border-radius: var(--radius-lg);
    font-size: 14px;
    line-height: 1.55;
    word-break: break-word;
  }

  /* Markdown styles scoped to bubbles */
  .message-bubble :global(p)          { margin: 0 0 0.5em; }
  .message-bubble :global(p:last-child) { margin-bottom: 0; }
  .message-bubble :global(code)       { font-family: var(--font-mono); font-size: 12px; background: rgba(0,0,0,0.25); padding: 1px 4px; border-radius: 3px; }
  .message-bubble :global(pre)        { background: rgba(0,0,0,0.35); padding: 10px 12px; border-radius: var(--radius-sm); overflow-x: auto; margin: 0.5em 0; }
  .message-bubble :global(pre code)   { background: none; padding: 0; }
  .message-bubble :global(ul), .message-bubble :global(ol) { padding-left: 1.4em; margin: 0.4em 0; }
  .message-bubble :global(blockquote) { border-left: 3px solid var(--accent-dim); padding-left: 10px; margin: 0.4em 0; color: var(--text-secondary); }
  .message-bubble :global(a)          { color: var(--accent); text-decoration: none; }
  .message-bubble :global(a:hover)    { text-decoration: underline; }
  .message-bubble :global(strong)     { color: var(--text-primary); }

  /* Typing dots for streaming indicator */
  .message-bubble :global(.typing-dot) {
    display: inline-block;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--text-muted);
    animation: bounce 1.2s ease-in-out infinite;
    margin-right: 4px;
  }

  .message-bubble :global(.typing-dot:nth-child(2)) { animation-delay: 0.2s; }
  .message-bubble :global(.typing-dot:nth-child(3)) { animation-delay: 0.4s; }

  @keyframes bounce {
    0%, 60%, 100% { transform: translateY(0); }
    30%            { transform: translateY(-5px); }
  }

  .message.user .message-bubble {
    background: var(--user-bubble);
    border-bottom-right-radius: var(--radius-sm);
    color: var(--text-primary);
  }

  .message.agent .message-bubble {
    background: var(--agent-bubble);
    border: 1px solid var(--border);
    border-bottom-left-radius: var(--radius-sm);
    color: var(--text-primary);
  }

  .message-time {
    font-size: 11px;
    color: var(--text-muted);
  }

  :global(.error-text) { color: var(--danger); }

  /* ── Input Form ───────────────────────────────────────────────────────── */
  .chat-form {
    display: flex;
    align-items: flex-end;
    gap: 10px;
    padding: 12px 20px 16px;
    border-top: 1px solid var(--border);
    background: var(--bg-surface);
    flex-shrink: 0;
  }

  .input-wrapper {
    flex: 1;
    position: relative;
  }

  .input-wrapper.command-mode::before {
    content: '/';
    position: absolute;
    left: 14px;
    top: 50%;
    transform: translateY(-50%);
    color: var(--accent);
    font-family: var(--font-mono);
    font-size: 14px;
    pointer-events: none;
    z-index: 1;
  }

  .chat-input {
    width: 100%;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    color: var(--text-primary);
    font-family: var(--font-sans);
    font-size: 14px;
    line-height: 1.5;
    padding: 10px 14px;
    resize: none;
    max-height: 140px;
    overflow-y: auto;
    outline: none;
    transition: border-color var(--transition);
    box-sizing: border-box;
  }

  .chat-input:focus { border-color: var(--accent); }
  .chat-input::placeholder { color: var(--text-muted); }
  .chat-input:disabled { opacity: 0.6; cursor: not-allowed; }
  .chat-input::-webkit-scrollbar { width: 4px; }
  .chat-input::-webkit-scrollbar-thumb { background: var(--border); border-radius: 2px; }

  .chat-input.vi-normal {
    border-color: var(--accent);
    background: var(--accent-dim);
  }

  .send-btn {
    background: var(--accent);
    border: none;
    border-radius: var(--radius-md);
    color: #fff;
    cursor: pointer;
    font-size: 16px;
    padding: 10px 14px;
    transition: background var(--transition), transform var(--transition);
    flex-shrink: 0;
    height: 40px;
  }

  .send-btn:hover   { background: var(--accent-hover); }
  .send-btn:active  { transform: scale(0.96); }
  .send-btn:disabled { background: var(--accent-dim); cursor: not-allowed; }
</style>
