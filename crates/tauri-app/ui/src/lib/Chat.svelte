<script>
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;

  let { settingsOpen = $bindable(false), proceduresOpen = $bindable(false), agentName = 'Pares Agens' } = $props();

  /** @type {{ role: 'user' | 'agent', content: string, time: string }[]} */
  let messages = $state([]);
  let inputValue = $state('');
  let busy = $state(false);
  let typing = $state(false);

  /** Format a Date as HH:MM */
  function fmtTime(date) {
    return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }

  async function sendMessage() {
    const content = inputValue.trim();
    if (!content || busy) return;

    inputValue = '';
    busy = true;
    typing = true;

    messages = [...messages, { role: 'user', content, time: fmtTime(new Date()) }];

    try {
      const response = await invoke('send_message', { content });
      typing = false;
      if (response) {
        messages = [...messages, { role: 'agent', content: response, time: fmtTime(new Date()) }];
      }
    } catch (err) {
      typing = false;
      messages = [...messages, { role: 'agent', content: `⚠ Error: ${err}`, time: fmtTime(new Date()) }];
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

  /** Open settings from system-tray "Settings" menu item. */
  $effect(() => {
    const unlisten = listen('show-settings', () => { settingsOpen = true; });
    return () => { unlisten.then(fn => fn?.()); };
  });
</script>

<main class="chat-panel">
  <header class="chat-header">
    <span class="status-dot {busy ? 'thinking' : 'online'}" title={busy ? 'Thinking' : 'Online'}></span>
    <h1>{agentName}</h1>
    <nav class="header-nav">
      <button class="icon-btn" title="Procedures" aria-haspopup="dialog"
        onclick={() => { proceduresOpen = true; }}>⚡</button>
      <button class="icon-btn" title="Settings" aria-haspopup="dialog"
        onclick={() => { settingsOpen = true; }}>⚙</button>
    </nav>
  </header>

  <section class="message-list" role="log" aria-live="polite" aria-label="Conversation">
    {#if messages.length === 0}
      <div class="welcome-message">
        <p>Hello! I'm <strong>{agentName}</strong>. How can I help you today?</p>
      </div>
    {/if}
    {#each messages as msg (msg)}
      <div class="message {msg.role}">
        <span class="message-sender">{msg.role === 'user' ? 'You' : agentName}</span>
        <div class="message-bubble">{msg.content}</div>
        <span class="message-time">{msg.time}</span>
      </div>
    {/each}
    {#if typing}
      <div class="message agent typing-indicator">
        <span class="message-sender">{agentName}</span>
        <div class="message-bubble">
          <span class="typing-dot"></span>
          <span class="typing-dot"></span>
          <span class="typing-dot"></span>
        </div>
      </div>
    {/if}
  </section>

  <form class="chat-form" autocomplete="off"
    onsubmit={(e) => { e.preventDefault(); sendMessage(); }}>
    <textarea
      class="chat-input"
      placeholder="Type a message… (Enter to send, Shift+Enter for newline)"
      rows="1"
      aria-label="Message input"
      bind:value={inputValue}
      onkeydown={handleKeydown}
    ></textarea>
    <button type="submit" class="send-btn" title="Send message" disabled={busy}>
      <span class="send-icon">➤</span>
    </button>
  </form>
</main>
