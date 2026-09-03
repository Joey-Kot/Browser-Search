import { BridgeClient } from "../src/bridge";

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) {
    throw new Error(message);
  }
}

const persistedStates: string[] = [];
Object.defineProperty(globalThis, "chrome", {
  configurable: true,
  value: {
    storage: {
      local: {
        set(values: { bridgeConnectionState: string }): Promise<void> {
          persistedStates.push(values.bridgeConnectionState);
          return Promise.resolve();
        }
      }
    }
  }
});

const bridge = new BridgeClient();
let completeDisconnectCleanup: (() => void) | undefined;
bridge.onState((state) => {
  if (state !== "disconnected") {
    return;
  }
  return new Promise<void>((resolve) => {
    completeDisconnectCleanup = resolve;
  });
});

const setState = (bridge as unknown as {
  setState(state: "connected" | "disconnected"): void;
}).setState.bind(bridge);

setState("connected");
setState("disconnected");

assert(
  persistedStates.join(",") === "connected,disconnected",
  "disconnect state must persist before asynchronous cleanup completes"
);
assert(
  completeDisconnectCleanup !== undefined,
  "disconnect cleanup handler was not invoked"
);
completeDisconnectCleanup();
