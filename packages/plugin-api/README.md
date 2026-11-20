# tauri-plugin-delta-update-api

Frontend types and `invoke` wrappers for `tauri-plugin-delta-update`.

```bash
pnpm add tauri-plugin-delta-update-api
```

```ts
import { createUpdateController, describeUpdate } from 'tauri-plugin-delta-update-api';

const updater = createUpdateController();
updater.subscribe((state) => {
  const view = describeUpdate(state);
  if (view.isReady) {
    console.log(`v${state?.version} ready on next launch`);
  }
  if (view.isRolledBack) {
    console.log(`rolled back to v${state?.version}`);
  }
});

await updater.start();

const result = await updater.check();
if (result.available) {
  console.log(result.notes, result.mandatory);
  await updater.prepare();
}
```

`describeUpdate` is the only place that encodes "what can the UI do now": `isReady`, `canPrepare`, `canRestart`, `canCancel`, `isBusy`, `isUpdateRequired` and `error`. Gate buttons on those flags instead of comparing state strings (`canCancel` is true only in `Downloading`/`Preparing`, where `cancel_prepare` actually stops work). React 18+ can use the optional hook subpath; its return value already contains the same flags:

```tsx
import { useUpdater } from 'tauri-plugin-delta-update-api/react';

function UpdateBanner() {
  const { state, isReady, restart } = useUpdater();
  return isReady
    ? <button onClick={() => void restart()}>Restart into v{state?.version}</button>
    : null;
}
```

`confirm()` is the post-restart health acknowledgement (state `Committed`). It is
**not** called automatically by default (`auto_confirm: false`); call it after
your app's core startup work succeeds. Set `auto_confirm: true` only if the
window being visible is a sufficient health signal for your app. `skip()`
implements “later”: it hides the version found by the last check until a newer
one appears, and is refused for mandatory updates or while an update is already
prepared (cancel first). The loop is closed by `check()` returning
`skipped_version`:

```ts
const result = await updater.check();
if (result.available) {
  await updater.prepare();
} else if (result.skipped_version) {
  // "仍然更新": clear the marker, then check again to get `available: true`.
  await updater.clearSkip();
  await updater.check();
}
```

When several components share one controller,
`start()`/`stop()` are reference counted and the listener is only released by
the last consumer. React is an optional peer dependency: Vue/Svelte and vanilla
apps use `createUpdateController` directly.

### Vue / Svelte

The controller already satisfies Svelte's store contract (`subscribe` emits the
current value immediately and returns an unsubscribe function), so `$updater`
works without an extra adapter:

```svelte
<script>
  import { onDestroy, onMount } from 'svelte';
  import { createUpdateController, describeUpdate } from 'tauri-plugin-delta-update-api';

  const updater = createUpdateController();
  $: view = describeUpdate($updater);
  onMount(() => void updater.start());
  onDestroy(() => updater.stop());
</script>

{#if view.isReady}
  <button on:click={() => updater.restart()}>Restart into v{$updater.version}</button>
{/if}
```

Vue 3 composition API:

```vue
<script setup lang="ts">
import { computed, onScopeDispose, shallowRef } from 'vue';
import { createUpdateController, describeUpdate } from 'tauri-plugin-delta-update-api';

const updater = createUpdateController();
const state = shallowRef(updater.getState());
const view = computed(() => describeUpdate(state.value));
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
  <button v-if="view.isReady" @click="updater.restart()">
    Restart into v{{ state?.version }}
  </button>
</template>
```

The low-level `check`, `prepare`, `STATE_EVENT` and typed payloads are still
exported. The package contains no update logic; verification, download and
replacement are performed by the Rust engine.
