import { describe, expect, it } from 'vitest';

import {
  describeUpdate,
  isBusy,
  isCommitted,
  isFailed,
  isPrepared,
  isRolledBack,
  isUpdateRequired,
  type UpdateStateName,
  type UpdateStatePayload,
} from '../src';

function payload(state: UpdateStateName, extra: Partial<UpdateStatePayload> = {}): UpdateStatePayload {
  return {
    v: 1,
    state,
    version: null,
    progress: null,
    error: null,
    timestamp: '2026-11-16T00:00:00Z',
    ...extra,
  };
}

const failure = {
  code: 'E_UPDATE_LOCKED',
  category: 'disk',
  message: 'locked',
  retryable: true,
  action: 'retry' as const,
  user_action: '稍后重试',
  detail: '',
};

describe('state guards', () => {
  it('only treats Prepared with a version as ready', () => {
    expect(isPrepared(payload('Prepared', { version: '2.0.0' }))).toBe(true);
    expect(isPrepared(payload('Prepared'))).toBe(false);
    expect(isPrepared(payload('Downloading', { version: '2.0.0' }))).toBe(false);
  });

  it('requires a version for Committed and reports RolledBack by name', () => {
    expect(isCommitted(payload('Committed', { version: '2.0.0' }))).toBe(true);
    expect(isCommitted(payload('Committed'))).toBe(false);
    expect(isCommitted(null)).toBe(false);
    expect(isRolledBack(payload('RolledBack'))).toBe(true);
    expect(isRolledBack(payload('Failed', { error: failure }))).toBe(false);
  });

  it('requires an error payload for Failed', () => {
    expect(isFailed(payload('Failed', { error: failure }))).toBe(true);
    // A Failed state without the payload is a producer bug, not a UI signal.
    expect(isFailed(payload('Failed'))).toBe(false);
    expect(isFailed(null)).toBe(false);
  });

  it('requires a minimum version for UpdateRequired', () => {
    expect(
      isUpdateRequired(
        payload('UpdateRequired', { min_supported_version: '1.4.0' }),
      ),
    ).toBe(true);
    expect(isUpdateRequired(payload('UpdateRequired'))).toBe(false);
  });

  it('treats only in-flight states as busy', () => {
    for (const state of ['Checking', 'Downloading', 'Preparing', 'Committing', 'RollingBack'] as const) {
      expect(isBusy(payload(state))).toBe(true);
    }
    for (const state of ['Idle', 'Prepared', 'Committed', 'Failed', 'RolledBack'] as const) {
      expect(isBusy(payload(state))).toBe(false);
    }
    expect(isBusy(null)).toBe(false);
  });
});

describe('describeUpdate', () => {
  it('reports nothing to do before the first state arrives', () => {
    const view = describeUpdate(null);
    expect(view.canPrepare).toBe(false);
    expect(view.canRestart).toBe(false);
    expect(view.isBusy).toBe(false);
    expect(view.error).toBeNull();
  });

  it('offers restart only for a prepared update', () => {
    const ready = describeUpdate(payload('Prepared', { version: '2.0.0' }));
    expect(ready.isReady).toBe(true);
    expect(ready.canRestart).toBe(true);
    expect(ready.canPrepare).toBe(false);

    const committing = describeUpdate(payload('Committing', { version: '2.0.0' }));
    expect(committing.isBusy).toBe(true);
    expect(committing.canRestart).toBe(false);
  });

  it('offers prepare from Idle, UpdateRequired and Failed', () => {
    expect(describeUpdate(payload('Idle')).canPrepare).toBe(true);
    expect(
      describeUpdate(payload('UpdateRequired', { min_supported_version: '1.4.0' }))
        .canPrepare,
    ).toBe(true);
    const failed = describeUpdate(payload('Failed', { error: failure }));
    expect(failed.canPrepare).toBe(true);
    expect(failed.error?.code).toBe('E_UPDATE_LOCKED');
  });

  it('offers cancel only while a prepare is actually running', () => {
    expect(describeUpdate(payload('Downloading')).canCancel).toBe(true);
    expect(describeUpdate(payload('Preparing')).canCancel).toBe(true);
    // Checking/Committing/RollingBack are busy but cancel does nothing there.
    for (const state of ['Idle', 'Checking', 'Committing', 'RollingBack', 'Prepared'] as const) {
      expect(describeUpdate(payload(state)).canCancel).toBe(false);
    }
    expect(describeUpdate(null).canCancel).toBe(false);
  });
});
