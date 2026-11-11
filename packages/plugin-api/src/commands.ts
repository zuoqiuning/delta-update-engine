import { invoke } from '@tauri-apps/api/core';

import type {
  CancelResult,
  CheckResult,
  ConfirmResult,
  PrepareResult,
  RestartResult,
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
