export interface BridgeSettings {
  bridgeUrl: string;
  extensionToken: string;
}

export interface PersistedJob {
  requestId: string;
  tabId: number;
  createdAt: number;
}

const SETTINGS_KEY = "bridgeSettings";
const INSTANCE_KEY = "browserInstanceId";
const JOB_PREFIX = "activeJob:";

const DEFAULT_SETTINGS: BridgeSettings = {
  bridgeUrl: "ws://127.0.0.1:17331/bridge",
  extensionToken: ""
};

export async function getBridgeSettings(): Promise<BridgeSettings> {
  const stored = await chrome.storage.local.get(SETTINGS_KEY);
  const value = stored[SETTINGS_KEY] as Partial<BridgeSettings> | undefined;
  return {
    bridgeUrl:
      typeof value?.bridgeUrl === "string" && value.bridgeUrl.length > 0
        ? value.bridgeUrl
        : DEFAULT_SETTINGS.bridgeUrl,
    extensionToken:
      typeof value?.extensionToken === "string" ? value.extensionToken : ""
  };
}

export async function setBridgeSettings(settings: BridgeSettings): Promise<void> {
  await chrome.storage.local.set({ [SETTINGS_KEY]: settings });
}

export async function getBrowserInstanceId(): Promise<string> {
  const stored = await chrome.storage.local.get(INSTANCE_KEY);
  const existing = stored[INSTANCE_KEY];
  if (typeof existing === "string" && existing.length > 0) {
    return existing;
  }
  const created = "chrome-profile-" + crypto.randomUUID();
  await chrome.storage.local.set({ [INSTANCE_KEY]: created });
  return created;
}

export async function persistJob(job: PersistedJob): Promise<void> {
  await chrome.storage.session.set({ [JOB_PREFIX + job.requestId]: job });
}

export async function removePersistedJob(requestId: string): Promise<void> {
  await chrome.storage.session.remove(JOB_PREFIX + requestId);
}

export async function listPersistedJobs(): Promise<PersistedJob[]> {
  const stored = await chrome.storage.session.get(null);
  return Object.entries(stored)
    .filter(([key]) => key.startsWith(JOB_PREFIX))
    .map(([, value]) => value)
    .filter(isPersistedJob);
}

function isPersistedJob(value: unknown): value is PersistedJob {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const job = value as Partial<PersistedJob>;
  return (
    typeof job.requestId === "string" &&
    typeof job.tabId === "number" &&
    typeof job.createdAt === "number"
  );
}
