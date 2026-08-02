import {
  getBridgeSettings,
  getBrowserInstanceId,
  setBridgeSettings
} from "../storage";

const form = required<HTMLFormElement>("#settings-form");
const bridgeUrl = required<HTMLInputElement>("#bridge-url");
const extensionToken = required<HTMLInputElement>("#extension-token");
const connectionState = required<HTMLElement>("#connection-state");
const instanceId = required<HTMLElement>("#instance-id");
const extensionVersion = required<HTMLElement>("#extension-version");
const message = required<HTMLElement>("#message");

form.addEventListener("submit", (event) => {
  event.preventDefault();
  void save();
});

void initialize();

async function initialize(): Promise<void> {
  const [settings, browserInstanceId, state] = await Promise.all([
    getBridgeSettings(),
    getBrowserInstanceId(),
    chrome.storage.local.get("bridgeConnectionState")
  ]);
  bridgeUrl.value = settings.bridgeUrl;
  extensionToken.value = settings.extensionToken;
  instanceId.textContent = browserInstanceId;
  extensionVersion.textContent = chrome.runtime.getManifest().version;
  updateConnectionState(state.bridgeConnectionState);
}

async function save(): Promise<void> {
  try {
    const url = new URL(bridgeUrl.value.trim());
    if (url.protocol !== "ws:" && url.protocol !== "wss:") {
      throw new Error("Bridge URL 必须使用 ws:// 或 wss://");
    }
    await setBridgeSettings({
      bridgeUrl: url.toString(),
      extensionToken: extensionToken.value
    });
    message.dataset.kind = "success";
    message.textContent = "设置已保存，扩展正在重载。";
    setTimeout(() => chrome.runtime.reload(), 250);
  } catch (error) {
    message.dataset.kind = "error";
    message.textContent = error instanceof Error ? error.message : String(error);
  }
}

function updateConnectionState(value: unknown): void {
  const state =
    value === "connected" || value === "connecting" ? value : "disconnected";
  connectionState.dataset.state = state;
  connectionState.textContent =
    state === "connected"
      ? "已连接"
      : state === "connecting"
        ? "正在连接"
        : "未连接";
}

function required<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector);
  if (!element) {
    throw new Error("Popup element is missing: " + selector);
  }
  return element;
}
