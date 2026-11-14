import {
  createUpdateController,
  isPrepared,
  type UpdateStatePayload,
} from 'tauri-plugin-delta-update-api';

const statusLine = document.querySelector<HTMLParagraphElement>('#status');
const restart = document.querySelector<HTMLButtonElement>('#restart');
const controller = createUpdateController();

/** Placeholder only: a real app should confirm after its own init completes. */
const CONFIRM_DELAY_MS = 3000;

function render(payload: UpdateStatePayload | null): void {
  if (!statusLine || !payload) return;

  if (isPrepared(payload)) {
    statusLine.textContent = `v${payload.version} 已准备好，下次启动生效`;
    if (restart) restart.hidden = false;
  } else if (payload.state === 'Committed') {
    statusLine.textContent = `已更新到 v${payload.version}`;
  } else if (payload.state === 'RolledBack') {
    statusLine.textContent = `新版本启动异常，已回滚到 v${payload.version ?? '旧版本'}`;
  } else if (payload.state === 'Failed') {
    statusLine.textContent = payload.error?.user_action ?? '更新失败';
  }
}

/** 订阅状态事件并接线到页面元素；应用每次启动只需调用一次。 */
export function mountUpdateUi(): void {
  controller.subscribe(render);
  controller.subscribe((payload) => {
    if (payload?.state === 'Committed') {
      // The app has just restarted into the new version. This fixed delay is a
      // placeholder: a real app must call confirm() after its critical
      // initialization (user data loaded, main window ready), not on a timer.
      window.setTimeout(() => void controller.confirm(), CONFIRM_DELAY_MS);
    }
  });
  void controller.start();

  restart?.addEventListener('click', () => {
    void controller.restart();
  });
}
