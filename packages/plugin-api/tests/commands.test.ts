import { describe, expect, it } from 'vitest';

import { isPrepared, STATE_EVENT, STATE_NAMES } from '../src';

describe('state protocol', () => {
  it('uses the documented event name', () => {
    expect(STATE_EVENT).toBe('delta-update://state');
  });

  it('contains every state that the plugin can emit', () => {
    expect(STATE_NAMES).toContain('Prepared');
    expect(STATE_NAMES).toContain('Cancelled');
    expect(STATE_NAMES).toContain('RolledBack');
    expect(STATE_NAMES).toContain('Failed');
  });

  it('treats Prepared without a version as not ready', () => {
    expect(
      isPrepared({
        v: 1,
        state: 'Prepared',
        version: null,
        progress: null,
        error: null,
        timestamp: '',
      }),
    ).toBe(false);
  });
});
