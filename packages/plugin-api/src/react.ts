import { useEffect, useMemo, useSyncExternalStore } from 'react';

import { createUpdateController, type UpdateController } from './controller';
import type {
  CancelResult,
  CheckResult,
  ConfirmResult,
  PrepareResult,
  RestartResult,
  UpdateStatePayload,
} from './types';

export interface UseUpdaterResult {
  state: UpdateStatePayload | null;
  controller: UpdateController;
  check: () => Promise<CheckResult>;
  prepare: () => Promise<PrepareResult>;
  cancel: () => Promise<CancelResult>;
  confirm: () => Promise<ConfirmResult>;
  restart: () => Promise<RestartResult>;
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
  };
}
