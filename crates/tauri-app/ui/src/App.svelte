<script>
  import Chat from './lib/Chat.svelte';
  import MemorySidebar from './lib/MemorySidebar.svelte';
  import Settings from './lib/Settings.svelte';
  import Procedures from './lib/Procedures.svelte';

  let settingsOpen = $state(false);
  let proceduresOpen = $state(false);
</script>

<MemorySidebar />
<Chat bind:settingsOpen bind:proceduresOpen />
<Settings bind:open={settingsOpen} />
<Procedures bind:open={proceduresOpen} />
