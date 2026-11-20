import {
  createUpdateController,
  isPrepared,
  type UpdateStatePayload,
} from 'tauri-plugin-delta-update-api';

const statusLine = document.querySelector<HTMLParagraphElement>('#status');
const restart = document.querySelector<HTMLButtonElement>('#restart');
const controller = createUpdateController();

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
  void controller.start();

  restart?.addEventListener('click', () => {
    void controller.restart();
  });
}

/**
 * 在应用自己的初始化完成之后调用（用户数据已加载、主界面可用），
 * 不要用固定延时代替健康判断，否则坏更新会被误判为健康。
 *
 * 需要 `delta-update:manage-confirm`（`init` 默认写入）；`manage` 不含 `confirm_healthy`。
 * 如果保持 `manage`，请改用 `auto_confirm: true`、Rust 侧 `confirm_healthy` 或在 capability 中显式授权。
 */
export function confirmWhenReady(): void {
  void controller.confirm();
}
