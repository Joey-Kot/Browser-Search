import { BridgeClient } from "./bridge";
import { JobManager } from "./jobs";

const bridge = new BridgeClient();
const jobs = new JobManager(bridge);

bridge.onMessage((message) => jobs.handleMessage(message));
bridge.onState((state) => {
  if (state === "disconnected") {
    void jobs.handleDisconnect();
  }
});

const initialized = jobs.initialize();

function startBridge(): void {
  void initialized.then(() => bridge.start());
}

// Browser Extractor registers tabs.onCreated when its JobManager is constructed.
// Keep the same wake-up event here: timers and stored connection state cannot
// restart a suspended worker. Register before asynchronous initialization ends.
chrome.tabs.onCreated.addListener(startBridge);

startBridge();
