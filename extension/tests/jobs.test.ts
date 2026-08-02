import type { BridgeClient } from "../src/bridge";
import { JobManager } from "../src/jobs";
import {
  PROTOCOL_VERSION,
  type IncomingMessage,
  type OutgoingMessage,
  type SearchCommand
} from "../src/types";

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) {
    throw new Error(message);
  }
}

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T): void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

interface ChromeTestState {
  createdTabs: number;
  attachedTabs: number[];
  detachedTabs: number[];
  removedTabs: number[];
  removedJobKeys: string[];
}

function installChrome(options: {
  createTab: () => Promise<chrome.tabs.Tab>;
  attach: (target: chrome.debugger.Debuggee) => Promise<void>;
}): ChromeTestState {
  const state: ChromeTestState = {
    createdTabs: 0,
    attachedTabs: [],
    detachedTabs: [],
    removedTabs: [],
    removedJobKeys: []
  };
  const eventListeners = new Set<
    (
      source: chrome.debugger.Debuggee,
      method: string,
      params?: object
    ) => void
  >();

  Object.defineProperty(globalThis, "chrome", {
    configurable: true,
    value: {
      tabs: {
        async create(): Promise<chrome.tabs.Tab> {
          state.createdTabs += 1;
          return options.createTab();
        },
        async remove(tabId: number): Promise<void> {
          state.removedTabs.push(tabId);
        }
      },
      debugger: {
        async attach(target: chrome.debugger.Debuggee): Promise<void> {
          if (target.tabId !== undefined) {
            state.attachedTabs.push(target.tabId);
          }
          await options.attach(target);
        },
        async detach(target: chrome.debugger.Debuggee): Promise<void> {
          if (target.tabId !== undefined) {
            state.detachedTabs.push(target.tabId);
          }
        },
        async sendCommand(): Promise<Record<string, never>> {
          return {};
        },
        onEvent: {
          addListener(listener: (
            source: chrome.debugger.Debuggee,
            method: string,
            params?: object
          ) => void): void {
            eventListeners.add(listener);
          },
          removeListener(listener: (
            source: chrome.debugger.Debuggee,
            method: string,
            params?: object
          ) => void): void {
            eventListeners.delete(listener);
          }
        }
      },
      storage: {
        session: {
          async set(): Promise<void> {},
          async remove(key: string): Promise<void> {
            state.removedJobKeys.push(key);
          }
        }
      }
    }
  });

  return state;
}

function createManager(
  messages: OutgoingMessage[] = [],
  isConnected: () => boolean = () => true
): JobManager {
  const bridge = {
    isConnected,
    send: (message: OutgoingMessage) => messages.push(message)
  };
  return new JobManager(bridge as unknown as BridgeClient);
}

const command: SearchCommand = {
  kind: "web",
  query: "Tokyo",
  start: 0,
  limit: 10,
  timeoutMs: 60_000,
  loadTimeoutMs: 30_000,
  selectorTimeoutMs: 10_000,
  url: "https://www.google.com/search?q=Tokyo",
  extraction: {
    rootSelectors: ["[data-result]"],
    dedupeField: "url",
    fields: {}
  }
};

function searchMessage(requestId: string): IncomingMessage {
  return {
    version: PROTOCOL_VERSION,
    type: "search",
    requestId,
    payload: command
  };
}

function cancelMessage(requestId: string): IncomingMessage {
  return {
    version: PROTOCOL_VERSION,
    type: "cancel",
    requestId,
    payload: {}
  };
}

function welcomeMessage(minOperationIntervalMs: number): IncomingMessage {
  return {
    version: PROTOCOL_VERSION,
    type: "welcome",
    payload: {
      protocolVersion: PROTOCOL_VERSION,
      sessionId: "test-session",
      pingIntervalSeconds: 20,
      minOperationIntervalMs
    }
  };
}

async function waitUntil(
  predicate: () => boolean,
  failureMessage: string
): Promise<void> {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    if (predicate()) {
      return;
    }
    await Promise.resolve();
  }
  throw new Error(failureMessage);
}

{
  const createGate = deferred<chrome.tabs.Tab>();
  const state = installChrome({
    createTab: () => createGate.promise,
    attach: async () => undefined
  });
  const messages: OutgoingMessage[] = [];
  const manager = createManager(messages);
  await manager.handleMessage(searchMessage("cancel-during-create"));
  await waitUntil(
    () => state.createdTabs === 1,
    "tab creation did not start"
  );

  const cancellation = manager.handleMessage(
    cancelMessage("cancel-during-create")
  );
  createGate.resolve({ id: 41 } as chrome.tabs.Tab);
  await cancellation;

  assert(state.attachedTabs.length === 0, "cancelled tab was attached");
  assert(
    state.removedTabs.length === 1 && state.removedTabs[0] === 41,
    "tab created during cancellation was not removed"
  );
  assert(
    state.removedJobKeys.includes("activeJob:cancel-during-create"),
    "persisted job was not removed after cancellation"
  );
  assert(
    messages.some(
      (message) =>
        message.type === "cleanup_complete" &&
        message.requestId === "cancel-during-create"
    ),
    "cleanup completion was not reported after cancellation"
  );
}

{
  const attachGate = deferred<void>();
  const state = installChrome({
    createTab: async () => ({ id: 42 }) as chrome.tabs.Tab,
    attach: () => attachGate.promise
  });
  const messages: OutgoingMessage[] = [];
  const manager = createManager(messages, () => false);
  await manager.handleMessage(searchMessage("disconnect-during-attach"));
  await waitUntil(
    () => state.attachedTabs.length === 1,
    "debugger attachment did not start"
  );

  const disconnection = manager.handleDisconnect();
  attachGate.resolve(undefined);
  await disconnection;

  assert(
    state.detachedTabs.length === 1 && state.detachedTabs[0] === 42,
    "debugger attached during disconnection was not detached"
  );
  assert(
    state.removedTabs.length === 1 && state.removedTabs[0] === 42,
    "tab attached during disconnection was not removed"
  );
  assert(
    !messages.some((message) => message.type === "cleanup_complete"),
    "disconnected cleanup should be confirmed by the next bridge connection"
  );
}

{
  const openedAt: number[] = [];
  let nextTabId = 100;
  const state = installChrome({
    createTab: async () => {
      openedAt.push(Date.now());
      return { id: nextTabId++ } as chrome.tabs.Tab;
    },
    attach: async () => undefined
  });
  const manager = createManager([], () => false);
  await manager.handleMessage(welcomeMessage(30));
  await manager.handleMessage(searchMessage("interval-first"));
  await manager.handleMessage(searchMessage("interval-second"));

  await new Promise((resolve) => setTimeout(resolve, 80));
  assert(state.createdTabs === 2, "both task tabs were not created");
  assert(
    (openedAt[1] ?? 0) - (openedAt[0] ?? 0) >= 25,
    "task tabs were opened without the configured global interval"
  );
  await manager.handleDisconnect();
}
