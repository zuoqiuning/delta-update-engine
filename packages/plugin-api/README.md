# tauri-plugin-delta-update-api

Frontend types and `invoke` wrappers for `tauri-plugin-delta-update`.

```bash
pnpm add tauri-plugin-delta-update-api
```

```ts
import { createUpdateController } from 'tauri-plugin-delta-update-api';

const updater = createUpdateController();
updater.subscribe((state) => {
  if (state?.state === 'Prepared') {
    console.log(`v${state.version} ready on next launch`);
  }
  if (state?.state === 'RolledBack') {
    console.log(`rolled back to v${state.version}`);
  }
});

await updater.start();

const result = await updater.check();
if (result.available) {
  console.log(result.notes, result.mandatory);
  await updater.prepare();
}
```

React 18+ can use the optional hook subpath:

```tsx
import { useUpdater } from 'tauri-plugin-delta-update-api/react';

function UpdateBanner() {
  const { state, restart } = useUpdater();
  return state?.state === 'Prepared'
    ? <button onClick={() => void restart()}>Restart into v{state.version}</button>
    : null;
}
```

`confirm()` is the post-restart health acknowledgement (state `Committed`); the
default `auto_confirm` already calls it. When several components share one
controller, `start()`/`stop()` are reference counted and the listener is only
released by the last consumer. React is an optional peer dependency: Vue/Svelte
and vanilla apps use `createUpdateController` directly.

### Vue / Svelte

The controller already satisfies Svelte's store contract (`subscribe` emits the
current value immediately and returns an unsubscribe function), so `$updater`
works without an extra adapter:

```svelte
<script>
  import { onDestroy, onMount } from 'svelte';
  import { createUpdateController } from 'tauri-plugin-delta-update-api';

  const updater = createUpdateController();
  onMount(() => void updater.start());
  onDestroy(() => updater.stop());
</script>

{#if $updater?.state === 'Prepared'}
  <button on:click={() => updater.restart()}>Restart into v{$updater.version}</button>
{/if}
```

Vue 3 composition API:

```vue
<script setup lang="ts">
import { onScopeDispose, shallowRef } from 'vue';
import { createUpdateController } from 'tauri-plugin-delta-update-api';

const updater = createUpdateController();
const state = shallowRef(updater.getState());
const unsubscribe = updater.subscribe((next) => {
  state.value = next;
});
void updater.start();
onScopeDispose(() => {
  unsubscribe();
  updater.stop();
});
</script>

<template>
  <button v-if="state?.state === 'Prepared'" @click="updater.restart()">
    Restart into v{{ state.version }}
  </button>
</template>
```

The low-level `check`, `prepare`, `STATE_EVENT` and typed payloads are still
exported. The package contains no update logic; verification, download and
replacement are performed by the Rust engine.
