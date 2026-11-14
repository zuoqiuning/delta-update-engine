import { invoke } from '@tauri-apps/api/core';

import type {
  CancelResult,
  CheckResult,
  ClearSkipResult,
  ConfirmResult,
  DiagnosticsResult,
  PrepareResult,
  RestartResult,
  UpdateRecord,
  UpdateStatePayload,
} from './types';

export function check(): Promise<CheckResult> {
  return invoke<CheckResult>('plugin:delta-update|check');
}

export function prepare(): Promise<PrepareResult> {
  return invoke<PrepareResult>('plugin:delta-update|prepare');
}

export function cancelPrepare(): Promise<CancelResult> {
  return invoke<CancelResult>('plugin:delta-update|cancel_prepare');
}

export function status(): Promise<UpdateStatePayload> {
  return invoke<UpdateStatePayload>('plugin:delta-update|status');
}

export function confirmHealthy(): Promise<ConfirmResult> {
  return invoke<ConfirmResult>('plugin:delta-update|confirm_healthy');
}

export function restartToApply(): Promise<RestartResult> {
  return invoke<RestartResult>('plugin:delta-update|restart_to_apply');
}

/** Write a support bundle next to the app logs and return its path. */
export function exportDiagnostics(): Promise<DiagnosticsResult> {
  return invoke<DiagnosticsResult>('plugin:delta-update|export_diagnostics');
}

/** Open the app log directory so the user can attach the newest bundle. */
export function openDiagnosticsFolder(): Promise<void> {
  return invoke<void>('plugin:delta-update|open_diagnostics_folder');
}

/** Remove the rollback skip marker so the same version can be retried. */
export function clearSkippedVersion(): Promise<ClearSkipResult> {
  return invoke<ClearSkipResult>('plugin:delta-update|clear_skipped_version');
}

/** Newest terminal update outcomes (completed / rolled_back / failed). */
export function getUpdateHistory(limit = 50): Promise<UpdateRecord[]> {
  return invoke<UpdateRecord[]>('plugin:delta-update|get_update_history', { limit });
}
