import { CdpSession } from "../src/cdp";
import { BrowserSearchError } from "../src/types";

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) {
    throw new Error(message);
  }
}

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
    debugger: {
      async attach(): Promise<void> {},
      async detach(): Promise<void> {},
      async sendCommand(
        _target: chrome.debugger.Debuggee,
        method: string
      ): Promise<Record<string, never>> {
        if (method === "Page.navigate") {
          throw new Error("navigation command failed");
        }
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
    }
  }
});

const session = new CdpSession(91);
await session.attach();

let failure: unknown;
try {
  await session.navigate(
    "https://example.com",
    60_000,
    new AbortController().signal
  );
} catch (error) {
  failure = error;
}

assert(failure instanceof BrowserSearchError, "navigation error was not wrapped");
const listeners = (
  session as unknown as {
    listeners: Map<string, Set<unknown>>;
  }
).listeners;
assert(listeners.size === 0, "failed navigation left its load waiter registered");

await session.detach();
