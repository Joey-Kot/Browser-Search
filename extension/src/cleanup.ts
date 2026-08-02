import { CdpSession } from "./cdp";
import { removePersistedJob } from "./storage";

export interface CleanupState {
  requestId: string;
  tabId: number | null;
  cdp: CdpSession | null;
  abortController: AbortController;
}

export async function cleanupJob(state: CleanupState): Promise<void> {
  state.abortController.abort();
  await state.cdp?.detach();
  if (state.tabId !== null) {
    await chrome.tabs.remove(state.tabId).catch(() => undefined);
  }
  await removePersistedJob(state.requestId);
}

