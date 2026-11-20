export const STATE_EVENT = 'delta-update://state';

export const STATE_NAMES = [
  'Idle',
  'UpdateRequired',
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
  /** Meaning of total/completed. `Prepared` payloads omit it. */
  unit?: 'bytes' | 'files' | 'ops';
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
  | 'unsupported'
  | 'restart';

export type ErrorCategory =
  | 'network'
  | 'signature'
  | 'manifest'
  | 'security'
  | 'disk'
  | 'permission'
  | 'patch'
  | 'journal'
  | 'rollback'
  | 'config'
  | 'platform'
  | 'apply'
  | 'internal';

export interface UpdateError {
  code: string;
  category: ErrorCategory;
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
  /** Set only in `UpdateRequired`: the oldest version the publisher supports. */
  min_supported_version?: string | null;
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
  /** The running version is below the publisher's `min_supported_version`. */
  update_required?: boolean;
  /** Oldest version the publisher still supports; absent when unset. */
  min_supported_version?: string | null;
  /**
   * Files skipped because they are cloud placeholders (OneDrive/Files
   * On-Demand). If the manifest lists them, they are downloaded in full.
   * Older plugin versions omit this field; treat a missing value as 0.
   */
  cloud_placeholders_skipped?: number;
  /**
   * Set when the manifest was suppressed because the user skipped this exact
   * version (`skipVersion()`). `available` stays `false`; call
   * `clearSkippedVersion()` to surface the update again.
   */
  skipped_version?: string | null;
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

export interface DiagnosticsResult {
  /** Absolute path of the JSON support bundle written next to the app logs. */
  path: string;
}

export interface ClearSkipResult {
  /** Whether a rollback skip marker existed and was removed. */
  cleared: boolean;
}

export interface SkipResult {
  /** Version now hidden from `check` until a newer release appears. */
  skipped: string;
}

/** One terminal update outcome from `get_update_history`. */
export interface UpdateRecord {
  v: number;
  version: string;
  from_version?: string | null;
  started_at_unix?: number | null;
  finished_at_unix: number;
  result: 'completed' | 'rolled_back' | 'failed';
  duration_ms?: number | null;
  reason?: string | null;
  error_code?: string | null;
}

export function isPrepared(
  payload: UpdateStatePayload,
): payload is UpdateStatePayload & { state: 'Prepared'; version: string } {
  return payload.state === 'Prepared' && payload.version !== null;
}

export const isCommitted = (
  payload: UpdateStatePayload | null,
): payload is UpdateStatePayload & { state: 'Committed'; version: string } =>
  payload?.state === 'Committed' && payload.version !== null;

export const isRolledBack = (
  payload: UpdateStatePayload | null,
): payload is UpdateStatePayload & { state: 'RolledBack' } =>
  payload?.state === 'RolledBack';

export const isFailed = (
  payload: UpdateStatePayload | null,
): payload is UpdateStatePayload & { state: 'Failed'; error: UpdateError } =>
  payload?.state === 'Failed' && payload.error !== null;

export const isUpdateRequired = (
  payload: UpdateStatePayload | null,
): payload is UpdateStatePayload & {
  state: 'UpdateRequired';
  min_supported_version: string;
} => payload?.state === 'UpdateRequired' && !!payload.min_supported_version;

const BUSY_STATES: ReadonlySet<UpdateStateName> = new Set([
  'Checking',
  'Downloading',
  'Preparing',
  'Committing',
  'RollingBack',
]);

/** The plugin is doing work; commands that need the same gate will fail. */
export const isBusy = (payload: UpdateStatePayload | null): boolean =>
  payload !== null && BUSY_STATES.has(payload.state);

/**
 * Everything a UI needs to decide what to render, derived once so every
 * framework agrees on the same rules.
 */
export interface UpdateView {
  isChecking: boolean;
  isDownloading: boolean;
  isReady: boolean;
  isCommitted: boolean;
  isRolledBack: boolean;
  isFailed: boolean;
  isBusy: boolean;
  isUpdateRequired: boolean;
  canPrepare: boolean;
  canRestart: boolean;
  /** A prepare is in flight and `cancel_prepare` will actually stop it. */
  canCancel: boolean;
  error: UpdateError | null;
}

export function describeUpdate(state: UpdateStatePayload | null): UpdateView {
  return {
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
    canCancel:
      state?.state === 'Downloading' || state?.state === 'Preparing',
    error: state?.error ?? null,
  };
}
