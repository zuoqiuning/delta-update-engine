import { useEffect, useMemo, useSyncExternalStore } from 'react';

import { createUpdateController, type UpdateController } from './controller';
import {
  isBusy,
  isCommitted,
  isFailed,
  isPrepared,
  isRolledBack,
  isUpdateRequired,
  type CancelResult,
  type CheckResult,
  type ConfirmResult,
  type PrepareResult,
  type RestartResult,
  type UpdateStatePayload,
} from './types';

export interface UseUpdaterResult {
  state: UpdateStatePayload | null;
  controller: UpdateController;
  check: () => Promise<CheckResult>;
  prepare: () => Promise<PrepareResult>;
  cancel: () => Promise<CancelResult>;
  confirm: () => Promise<ConfirmResult>;
  restart: () => Promise<RestartResult>;
  /** Prepare unless already prepared or busy; resolves `null` when skipped. */
  retryPrepare: () => Promise<PrepareResult | null>;
  /** Restart only when an update is prepared; resolves `null` otherwise. */
  retryRestart: () => Promise<RestartResult | null>;
  /** Derived flags so callers do not hand-compare state strings. */
  isChecking: boolean;
  isDownloading: boolean;
  isReady: boolean;
  isCommitted: boolean;
  isRolledBack: boolean;
  isFailed: boolean;
  isBusy: boolean;
  /** The running version is below the publisher's `min_supported_version`. */
  isUpdateRequired: boolean;
  canPrepare: boolean;
  canRestart: boolean;
  error: UpdateStatePayload['error'];
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
    retryPrepare: async () => {
      if (state !== null && (isPrepared(state) || isBusy(state))) return null;
      return controller.prepare();
    },
    retryRestart: async () => {
      if (state === null || !isPrepared(state)) return null;
      return controller.restart();
    },
    isChecking: state?.state === 'Checking',
    isDownloading: state?.state === 'Downloading' || state?.state === 'Preparing',
    isReady: state !== null && isPrepared(state),
    isCommitted: isCommitted(state),
    isRolledBack: isRolledBack(state),
    isFailed: isFailed(state),
    isBusy: isBusy(state),
    isUpdateRequired: isUpdateRequired(state),
    canPrepare:
      state?.state === 'Idle' ||
      state?.state === 'UpdateRequired' ||
      isFailed(state),
    canRestart: state !== null && isPrepared(state),
    error: state?.error ?? null,
  };
}
