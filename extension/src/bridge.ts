import {
  PROTOCOL_VERSION,
  parseIncomingMessage,
  type IncomingMessage,
  type OutgoingMessage
} from "./types";
import { getBridgeSettings, getBrowserInstanceId } from "./storage";

export type BridgeState = "disconnected" | "connecting" | "connected";
export type BridgeMessageHandler = (
  message: IncomingMessage
) => void | Promise<void>;
export type BridgeStateHandler = (
  state: BridgeState
) => void | Promise<void>;

export class BridgeClient {
  private socket: WebSocket | null = null;
  private reconnectAttempt = 0;
  private messageHandler: BridgeMessageHandler | null = null;
  private stateHandler: BridgeStateHandler | null = null;
  private state: BridgeState = "disconnected";

  onMessage(handler: BridgeMessageHandler): void {
    this.messageHandler = handler;
  }

  onState(handler: BridgeStateHandler): void {
    this.stateHandler = handler;
  }

  start(): void {
    void this.connect();
  }

  isConnected(): boolean {
    return this.socket?.readyState === WebSocket.OPEN;
  }

  send(message: OutgoingMessage): void {
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN) {
      throw new Error("Bridge is not connected");
    }
    this.socket.send(JSON.stringify(message));
  }

  private async connect(): Promise<void> {
    if (this.state === "connecting" || this.isConnected()) {
      return;
    }
    await this.setState("connecting");
    try {
      const [settings, browserInstanceId] = await Promise.all([
        getBridgeSettings(),
        getBrowserInstanceId()
      ]);
      const url = new URL(settings.bridgeUrl);
      if (url.protocol !== "ws:" && url.protocol !== "wss:") {
        throw new Error("Bridge URL must use ws:// or wss://");
      }

      const socket = new WebSocket(url.toString());
      this.socket = socket;
      socket.addEventListener("open", () => {
        this.reconnectAttempt = 0;
        this.send({
          version: PROTOCOL_VERSION,
          type: "hello",
          payload: {
            extensionToken: settings.extensionToken,
            browserInstanceId,
            browserName: "Chrome",
            extensionVersion: chrome.runtime.getManifest().version,
            protocolVersion: PROTOCOL_VERSION
          }
        });
        void this.setState("connected");
      });

      socket.addEventListener("message", (event) => {
        void this.handleIncoming(String(event.data));
      });

      socket.addEventListener("close", () => {
        void this.handleClose(socket);
      });

      socket.addEventListener("error", () => socket.close());
    } catch {
      await this.setState("disconnected");
      this.scheduleReconnect();
    }
  }

  private async handleClose(socket: WebSocket): Promise<void> {
    if (this.socket !== socket) {
      return;
    }
    this.socket = null;
    await this.setState("disconnected");
    this.scheduleReconnect();
  }

  private async handleIncoming(data: string): Promise<void> {
    let message: IncomingMessage;
    try {
      message = parseIncomingMessage(data);
    } catch {
      this.socket?.close();
      return;
    }

    if (message.type === "ping") {
      this.send({
        version: PROTOCOL_VERSION,
        type: "pong",
        payload: { nonce: message.payload.nonce ?? null }
      });
      return;
    }
    await this.messageHandler?.(message);
  }

  private scheduleReconnect(): void {
    const delay = Math.min(30_000, 500 * 2 ** this.reconnectAttempt);
    this.reconnectAttempt += 1;
    setTimeout(() => void this.connect(), delay);
  }

  private async setState(state: BridgeState): Promise<void> {
    if (this.state === state) {
      return;
    }
    this.state = state;
    await this.stateHandler?.(state);
    void chrome.storage.local.set({ bridgeConnectionState: state });
  }
}
