export const STATE_EVENT = 'delta-update://state';

export const STATE_NAMES = [
  'Idle',
  'Checking',
  'Downloading',
  'Preparing',
  'Prepared',
  'Cancelled',
  'Committing',
  'Committed',
  'RollingBack',
  'RolledBack',
  'Failed',
] as const;

export type UpdateStateName = (typeof STATE_NAMES)[number];

export interface UpdateProgress {
  total: number;
  completed: number;
  /** File currently being downloaded or verified, when the plugin reports one. */
  file?: string | null;
}

export type ErrorAction =
  | 'retry'
  | 'wait'
  | 'check_update'
  | 'contact_support'
  | 'grant_permission'
  | 'free_space'
  | 'update_client'
  | 'report_security'
  | 'recover_backup'
  | 'fix_config'
  | 'unsupported';

export interface UpdateError {
  code: string;
  category: string;
  message: string;
  retryable: boolean;
  action: ErrorAction;
  user_action: string;
  detail: string;
}

export interface UpdateStatePayload {
  v: number;
  state: UpdateStateName;
  version: string | null;
  progress: UpdateProgress | null;
  error: UpdateError | null;
  timestamp: string;
}

export interface CheckResult {
  available: boolean;
  version: string | null;
  current_version: string;
  files_changed: number;
  files_staged: number;
  download_bytes: number;
  total_bytes: number;
  notes: string | null;
  mandatory: boolean;
}

export interface PrepareResult {
  /** `"Prepared"` on completion; the current state name when a prepare is already running. */
  state: UpdateStateName;
  version: string;
}

export interface CancelResult {
  state: 'Cancelled';
}

export interface ConfirmResult {
  acknowledged: boolean;
}

export interface RestartResult {
  state: 'Committing';
  version: string;
}

export function isPrepared(
  payload: UpdateStatePayload,
): payload is UpdateStatePayload & { state: 'Prepared'; version: string } {
  return payload.state === 'Prepared' && payload.version !== null;
}
