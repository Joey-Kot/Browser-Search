function assert(condition: unknown, message: string): asserts condition {
  if (!condition) {
    throw new Error(message);
  }
}

const tabCreatedListeners: Array<() => void> = [];
const closedTabs: number[] = [];
let finishLoadingJobs: ((jobs: Record<string, unknown>) => void) | undefined;
const storedJobs = new Promise<Record<string, unknown>>((resolve) => {
  finishLoadingJobs = resolve;
});

class MockWebSocket {
  static readonly OPEN = 1;
  static readonly instances: MockWebSocket[] = [];
  readyState = 0;
  private readonly listeners = new Map<string, () => void>();

  constructor(readonly url: string) {
    MockWebSocket.instances.push(this);
  }

  addEventListener(type: string, handler: () => void): void {
    this.listeners.set(type, handler);
  }

  send(): void {}

  open(): void {
    this.readyState = MockWebSocket.OPEN;
    this.listeners.get("open")?.();
  }
}

Object.defineProperty(globalThis, "WebSocket", {
  configurable: true,
  value: MockWebSocket
});
Object.defineProperty(globalThis, "chrome", {
  configurable: true,
  value: {
    runtime: { getManifest: () => ({ version: "0.1.0" }) },
    tabs: {
      onCreated: {
        addListener(listener: () => void): void {
          tabCreatedListeners.push(listener);
        }
      },
      async remove(tabId: number): Promise<void> {
        closedTabs.push(tabId);
      }
    },
    storage: {
      local: {
        async get(): Promise<Record<string, unknown>> {
          return {
            browserInstanceId: "test-browser",
            bridgeSettings: {
              bridgeUrl: "ws://127.0.0.1:17331/bridge",
              extensionToken: "test-token"
            }
          };
        },
        async set(): Promise<void> {}
      },
      session: {
        get: () => storedJobs,
        async remove(): Promise<void> {}
      }
    }
  }
});

await import("../src/service-worker");

assert(
  tabCreatedListeners.length === 1,
  "the worker must register its tab wake-up listener before asynchronous initialization"
);
for (const listener of tabCreatedListeners) {
  listener();
}
await Promise.resolve();
assert(
  MockWebSocket.instances.length === 0,
  "a wake-up event must not connect before leftover task tabs are cleaned up"
);

assert(finishLoadingJobs !== undefined, "job initialization did not start");
finishLoadingJobs({
  "activeJob:leftover": {
    requestId: "leftover",
    tabId: 42,
    createdAt: 1
  }
});
for (let turn = 0; turn < 20; turn += 1) {
  await Promise.resolve();
}
assert(closedTabs.includes(42), "the leftover task tab was not closed");
assert(
  Number(MockWebSocket.instances.length) === 1,
  "startup and a concurrent tab event must share one bridge connection"
);
const socket = MockWebSocket.instances[0];
assert(socket !== undefined, "bridge connection was not created");
assert(
  socket.url === "ws://127.0.0.1:17331/bridge",
  "the Search bridge address must remain unchanged"
);
socket.open();
for (const listener of tabCreatedListeners) {
  listener();
  listener();
}
for (let turn = 0; turn < 10; turn += 1) {
  await Promise.resolve();
}
assert(
  Number(MockWebSocket.instances.length) === 1,
  "opening tabs while connected must not create duplicate bridge sessions"
);

export {};
