<script lang="ts">
  // Shown when the hub rejects our (missing) auth cookie.
</script>

<div class="notice">
  <h2>Not signed in</h2>
  <p>
    Open the UI from a terminal with <code>trun ui</code>. It passes your local token
    (<code>~/.trun/hub.token</code>) and this page will remember it.
  </p>
</div>

<style>
  .notice {
    max-width: 560px;
    margin: 60px auto;
    padding: 20px 24px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
  }
  h2 {
    margin: 0 0 8px;
    font-size: 17px;
  }
</style>
