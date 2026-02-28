<script>
  import Chat from "./lib/Chat.svelte";
  import MemorySidebar from "./lib/MemorySidebar.svelte";
  import Settings from "./lib/Settings.svelte";

  let settingsOpen = $state(false);
</script>

<div class="layout">
  <MemorySidebar />
  <Chat onOpenSettings={() => { settingsOpen = true; }} />
</div>

<Settings bind:open={settingsOpen} />

<style>
  .layout {
    display: grid;
    grid-template-columns: 240px 1fr;
    height: 100vh;
    overflow: hidden;
  }
</style>
