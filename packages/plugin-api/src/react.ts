import { useEffect, useMemo, useSyncExternalStore } from 'react';

import { createUpdateController, type UpdateController } from './controller';
import {
  describeUpdate,
  isBusy,
  isPrepared,
  type CancelResult,
  type CheckResult,
  type ClearSkipResult,
  type ConfirmResult,
  type PrepareResult,
  type RestartResult,
  type SkipResult,
  type UpdateStatePayload,
  type UpdateView,
} from './types';

export interface UseUpdaterResult extends UpdateView {
  state: UpdateStatePayload | null;
  controller: UpdateController;
  check: () => Promise<CheckResult>;
  prepare: () => Promise<PrepareResult>;
  cancel: () => Promise<CancelResult>;
  confirm: () => Promise<ConfirmResult>;
  restart: () => Promise<RestartResult>;
  skip: () => Promise<SkipResult>;
  clearSkip: () => Promise<ClearSkipResult>;
  /** Prepare unless already prepared or busy; resolves `null` when skipped. */
  retryPrepare: () => Promise<PrepareResult | null>;
  /** Restart only when an update is prepared; resolves `null` otherwise. */
  retryRestart: () => Promise<RestartResult | null>;
}

/**
 * React hook around `createUpdateController`.
 *
 * Pass a shared controller when several components must observe the same
 * state; the hook otherwise creates one per component. The controller is
 * started on mount and stopped on unmount.
 */
export function useUpdater(shared?: UpdateController): UseUpdaterResult {
  const controller = useMemo(() => shared ?? createUpdateController(), [shared]);
  const state = useSyncExternalStore(
    controller.subscribe,
    controller.getState,
    controller.getState,
  );

  useEffect(() => {
    void controller.start().catch((error) => {
      // The hook has no error channel; a failed start leaves `state` null.
      console.warn('[delta-update] failed to start the update controller', error);
    });
    return () => controller.stop();
  }, [controller]);

  return {
    state,
    controller,
    check: controller.check,
    prepare: controller.prepare,
    cancel: controller.cancel,
    confirm: controller.confirm,
    restart: controller.restart,
    skip: controller.skip,
    clearSkip: controller.clearSkip,
    retryPrepare: async () => {
      if (state !== null && (isPrepared(state) || isBusy(state))) return null;
      return controller.prepare();
    },
    retryRestart: async () => {
      if (state === null || !isPrepared(state)) return null;
      return controller.restart();
    },
    ...describeUpdate(state),
  };
}
