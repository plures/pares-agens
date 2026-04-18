<script>
  import Chat from './lib/Chat.svelte';
  import MemorySidebar from './lib/MemorySidebar.svelte';
  import Settings from './lib/Settings.svelte';
  import Procedures from './lib/Procedures.svelte';
  import Wizard from './lib/Wizard.svelte';

  const tauriCore = typeof window !== 'undefined' ? window.__TAURI__?.core : undefined;
  const tauriEvent = typeof window !== 'undefined' ? window.__TAURI__?.event : undefined;
  const invoke = tauriCore?.invoke;
  const listen = tauriEvent?.listen;

  /** @type {{ id: string, title: string, body: string, actions: { id: string, label: string }[] }[]} */
  let actionableNotifications = $state([]);
  let settingsOpen = $state(false);
  let proceduresOpen = $state(false);
  /** Agent display name — set by the wizard on first run, used in Chat. */
  let agentName = $state('Pares Agens');

  function handleWizardComplete(/** @type {string} */ name) {
    agentName = name;
  }

  function dismissNotification(id) {
    actionableNotifications = actionableNotifications.filter((n) => n.id !== id);
  }

  async function triggerNotificationAction(notificationId, action) {
    if (invoke) {
      try {
        await invoke('handle_notification_action', {
          notificationId,
          action
        });
      } catch (err) {
        console.warn('Failed to handle notification action:', err);
      }
    }
    dismissNotification(notificationId);
  }

  $effect(() => {
    if (!listen) return;
    const unlisten = listen('actionable-notification', (event) => {
      const payload = event.payload;
      if (!payload || !payload.id) return;
      actionableNotifications = [
        payload,
        ...actionableNotifications.filter((n) => n.id !== payload.id)
      ].slice(0, 3);
    });
    return () => {
      unlisten.then((fn) => fn?.());
    };
  });
</script>

{#if actionableNotifications.length > 0}
  <section class="actionable-notifications" aria-live="polite" aria-label="Actionable notifications">
    {#each actionableNotifications as notification (notification.id)}
      <article class="actionable-notification-card">
        <h3>{notification.title}</h3>
        <p>{notification.body}</p>
        <div class="actionable-notification-actions">
          {#each notification.actions as action (action.id)}
            <button
              type="button"
              class={`actionable-btn ${action.id === 'view' ? 'secondary' : 'primary'}`}
              onclick={() => triggerNotificationAction(notification.id, action.id)}>
              {action.label}
            </button>
          {/each}
          <button
            type="button"
            class="actionable-btn secondary"
            onclick={() => dismissNotification(notification.id)}>
            Dismiss
          </button>
        </div>
      </article>
    {/each}
  </section>
{/if}

<Wizard onComplete={handleWizardComplete} />
<MemorySidebar />
<Chat bind:settingsOpen bind:proceduresOpen {agentName} />
<Settings bind:open={settingsOpen} />
<Procedures bind:open={proceduresOpen} />
