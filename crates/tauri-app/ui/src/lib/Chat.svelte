<script>
  /** @type {{ onOpenSettings: () => void }} */
  const { onOpenSettings } = $props();

  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;

  // ── State ──────────────────────────────────────────────────────────────
  /** @type {Array<{role: "user"|"agent", content: string, time: string}>} */
  let messages = $state([]);
  let input = $state("");
  let isBusy = $state(false);

  // ── Helpers ────────────────────────────────────────────────────────────
  function fmtTime() {
    return new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }

  // ── Auto-grow textarea ─────────────────────────────────────────────────
  /** @param {HTMLTextAreaElement} el */
  function autoGrow(el) {
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 140) + "px";
  }

  // ── Scroll to bottom ───────────────────────────────────────────────────
  /** @type {HTMLElement | null} */
  let messageListEl = $state(null);
  function scrollToBottom() {
    if (messageListEl) messageListEl.scrollTop = messageListEl.scrollHeight;
  }

  $effect(() => {
    // Scroll whenever messages array changes.
    if (messages) scrollToBottom();
  });

  // ── Listen for tray "Settings" event ───────────────────────────────────
  $effect(() => {
    const unlisten = listen("show-settings", () => onOpenSettings()).catch(console.error);
    return () => { unlisten.then(fn => fn()); };
  });

  // ── Send message ───────────────────────────────────────────────────────
  async function sendMessage() {
    const content = input.trim();
    if (isBusy || !content) return;

    isBusy = true;
    input = "";
    messages = [...messages, { role: "user", content, time: fmtTime() }];

    try {
      const response = await invoke("send_message", { content });
      if (response) {
        messages = [...messages, { role: "agent", content: response, time: fmtTime() }];
      }
    } catch (err) {
      messages = [...messages, { role: "agent", content: `⚠ Error: ${err}`, time: fmtTime() }];
    } finally {
      isBusy = false;
    }
  }

  // ── Keyboard handler ───────────────────────────────────────────────────
  /** @param {KeyboardEvent} e */
  function onKeydown(e) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
  }
</script>

<main class="chat-panel">
  <header class="chat-header">
    <span
      class="status-dot"
      class:online={!isBusy}
      class:thinking={isBusy}
      title={isBusy ? "Thinking…" : "Online"}
    ></span>
    <h1>Pares Agens</h1>
    <nav class="header-nav">
      <button
        class="icon-btn"
        title="Settings"
        aria-haspopup="dialog"
        onclick={onOpenSettings}
      >⚙</button>
    </nav>
  </header>

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
    {/if}

    {#each messages as msg (msg)}
      <div class="message {msg.role}">
        <span class="message-sender">{msg.role === "user" ? "You" : "Pares Agens"}</span>
        <div class="message-bubble">{msg.content}</div>
        <span class="message-time">{msg.time}</span>
      </div>
    {/each}

    {#if isBusy}
      <div class="message agent typing-indicator">
        <span class="message-sender">Pares Agens</span>
        <div class="message-bubble">
          <span class="typing-dot"></span>
          <span class="typing-dot"></span>
          <span class="typing-dot"></span>
        </div>
      </div>
    {/if}
  </section>

  <form
    class="chat-form"
    autocomplete="off"
    onsubmit={(e) => { e.preventDefault(); sendMessage(); }}
  >
    <textarea
      class="chat-input"
      placeholder="Type a message… (Enter to send, Shift+Enter for newline)"
      rows="1"
      aria-label="Message input"
      required
      bind:value={input}
      oninput={(e) => autoGrow(e.currentTarget)}
      onkeydown={onKeydown}
    ></textarea>
    <button
      type="submit"
      class="send-btn"
      title="Send message"
      disabled={isBusy}
    >
      <span class="send-icon">➤</span>
    </button>
  </form>
</main>

<style>
  .chat-panel {
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--bg-base);
  }

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
  }

  .header-nav { display: flex; gap: 4px; }

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

  .message.user  { align-self: flex-end; align-items: flex-end; }
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
    white-space: pre-wrap;
    word-break: break-word;
  }

  .message.user  .message-bubble {
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

  .message-time { font-size: 11px; color: var(--text-muted); }

  .typing-indicator .message-bubble {
    display: flex;
    gap: 5px;
    padding: 12px 16px;
    align-items: center;
  }

  .chat-form {
    display: flex;
    align-items: flex-end;
    gap: 10px;
    padding: 12px 20px 16px;
    border-top: 1px solid var(--border);
    background: var(--bg-surface);
    flex-shrink: 0;
  }

  .chat-input {
    flex: 1;
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
  }

  .chat-input:focus { border-color: var(--accent); }
  .chat-input::placeholder { color: var(--text-muted); }

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

  .send-btn:hover  { background: var(--accent-hover); }
  .send-btn:active { transform: scale(0.96); }
  .send-btn:disabled { background: var(--accent-dim); cursor: not-allowed; }
</style>
