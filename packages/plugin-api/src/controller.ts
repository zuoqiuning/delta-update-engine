import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import {
  cancelPrepare,
  check,
  confirmHealthy,
  prepare,
  restartToApply,
  status,
} from './commands';
import { STATE_EVENT, type UpdateStatePayload } from './types';

export type UpdateStateListener = (state: UpdateStatePayload | null) => void;

/**
 * Framework-agnostic update store. It keeps the latest state event, exposes a
 * snapshot and wraps the six commands, so React/Vue/Svelte apps do not have to
 * duplicate listener and error plumbing.
 *
 * `start`/`stop` are reference counted: several components can share one
 * controller, and the event listener is only released when the last one stops.
 */
export interface UpdateController {
  getState(): UpdateStatePayload | null;
  subscribe(listener: UpdateStateListener): () => void;
  start(): Promise<void>;
  stop(): void;
  check: typeof check;
  prepare: typeof prepare;
  cancel: typeof cancelPrepare;
  confirm: typeof confirmHealthy;
  restart: typeof restartToApply;
}

export function createUpdateController(): UpdateController {
  let state: UpdateStatePayload | null = null;
  let unlisten: UnlistenFn | null = null;
  let starting: Promise<void> | null = null;
  let refs = 0;
  let generation = 0;
  const listeners = new Set<UpdateStateListener>();

  const notify = (listener: UpdateStateListener): void => {
    try {
      listener(state);
    } catch (error) {
      // One broken component must not starve the other subscribers.
      console.error('[delta-update] state listener failed', error);
    }
  };

  const update = (next: UpdateStatePayload): void => {
    state = next;
    for (const listener of [...listeners]) notify(listener);
  };

  const start = (): Promise<void> => {
    refs += 1;
    if (starting) return starting;
    const current = ++generation;
    starting = (async () => {
      // Subscribe before reading the snapshot: an update that lands in
      // between would otherwise be overwritten by the stale snapshot.
      let sawEvent = false;
      const stopListening = await listen<UpdateStatePayload>(STATE_EVENT, (event) => {
        sawEvent = true;
        update(event.payload);
      });
      if (current !== generation) {
        stopListening();
        return;
      }
      unlisten = stopListening;
      let initial: UpdateStatePayload;
      try {
        initial = await status();
      } catch (error) {
        stopListening();
        if (unlisten === stopListening) unlisten = null;
        throw error;
      }
      if (current === generation && !sawEvent) update(initial);
    })().catch((error) => {
      if (current === generation) {
        starting = null;
        refs = 0;
      }
      throw error;
    });
    return starting;
  };

  const stop = (): void => {
    refs = Math.max(0, refs - 1);
    if (refs > 0) return;
    generation += 1;
    unlisten?.();
    unlisten = null;
    starting = null;
  };

  return {
    getState: () => state,

    subscribe(listener) {
      listeners.add(listener);
      notify(listener);
      return () => {
        listeners.delete(listener);
      };
    },

    start,
    stop,

    check,
    prepare,
    cancel: cancelPrepare,
    confirm: confirmHealthy,
    restart: restartToApply,
  };
}
