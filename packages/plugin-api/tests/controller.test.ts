import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  unlisten: vi.fn(),
  status: vi.fn(),
  handler: undefined as ((event: { payload: unknown }) => void) | undefined,
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (_event: string, handler: (event: { payload: unknown }) => void) => {
    mocks.handler = handler;
    return mocks.unlisten;
  }),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (command: string) => {
    if (command === 'plugin:delta-update|status') return mocks.status();
    throw new Error(`unexpected command ${command}`);
  }),
}));

import { createUpdateController, type UpdateStatePayload } from '../src';

function snapshot(state: UpdateStatePayload['state']): UpdateStatePayload {
  return {
    v: 1,
    state,
    version: null,
    progress: null,
    error: null,
    timestamp: '',
  };
}

describe('update controller', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.handler = undefined;
  });

  it('loads a snapshot, forwards events and stops listening', async () => {
    mocks.status.mockResolvedValue(snapshot('Idle'));
    const controller = createUpdateController();
    const seen: Array<UpdateStatePayload['state'] | null> = [];
    controller.subscribe((state) => seen.push(state?.state ?? null));

    await controller.start();
    expect(controller.getState()?.state).toBe('Idle');

    mocks.handler?.({ payload: snapshot('Prepared') });
    expect(controller.getState()?.state).toBe('Prepared');
    expect(seen).toEqual([null, 'Idle', 'Prepared']);

    controller.stop();
    expect(mocks.unlisten).toHaveBeenCalledOnce();
  });

  it('prefers an event that arrives while the snapshot is loading', async () => {
    let resolveStatus!: (value: UpdateStatePayload) => void;
    mocks.status.mockReturnValue(
      new Promise<UpdateStatePayload>((resolve) => {
        resolveStatus = resolve;
      }),
    );
    const controller = createUpdateController();

    const started = controller.start();
    await Promise.resolve();
    mocks.handler?.({ payload: snapshot('Downloading') });
    resolveStatus(snapshot('Idle'));
    await started;

    expect(controller.getState()?.state).toBe('Downloading');
  });

  it('keeps notifying when one listener throws', async () => {
    mocks.status.mockResolvedValue(snapshot('Idle'));
    const controller = createUpdateController();
    const seen: string[] = [];
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    controller.subscribe(() => {
      throw new Error('boom');
    });
    controller.subscribe((state) => {
      if (state) seen.push(state.state);
    });

    await controller.start();
    mocks.handler?.({ payload: snapshot('Prepared') });

    expect(seen).toEqual(['Idle', 'Prepared']);
    expect(error).toHaveBeenCalled();
    error.mockRestore();
    controller.stop();
  });

  it('keeps listening until every starter has stopped', async () => {
    mocks.status.mockResolvedValue(snapshot('Idle'));
    const controller = createUpdateController();

    await controller.start();
    await controller.start();
    controller.stop();
    expect(mocks.unlisten).not.toHaveBeenCalled();

    controller.stop();
    expect(mocks.unlisten).toHaveBeenCalledOnce();
  });
});
