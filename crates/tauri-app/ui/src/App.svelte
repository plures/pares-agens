<script>
  import Chat from './lib/Chat.svelte';
  import MemorySidebar from './lib/MemorySidebar.svelte';
  import Settings from './lib/Settings.svelte';
  import Procedures from './lib/Procedures.svelte';
  import Wizard from './lib/Wizard.svelte';

  let settingsOpen = $state(false);
  let proceduresOpen = $state(false);
  /** Agent display name — set by the wizard on first run, used in Chat. */
  let agentName = $state('Pares Agens');

  function handleWizardComplete(/** @type {string} */ name) {
    agentName = name;
  }
</script>

<Wizard onComplete={handleWizardComplete} />
<MemorySidebar />
<Chat bind:settingsOpen bind:proceduresOpen {agentName} />
<Settings bind:open={settingsOpen} />
<Procedures bind:open={proceduresOpen} />
