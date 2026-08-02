import { BrowserSearchError } from "./types";

interface RuntimeResponse {
  result?: { value?: unknown };
  exceptionDetails?: {
    text?: string;
    exception?: { description?: string };
  };
}

type EventListener = (params: Record<string, unknown>) => void;

export class CdpSession {
  private readonly target: chrome.debugger.Debuggee;
  private readonly listeners = new Map<string, Set<EventListener>>();
  private attached = false;
  private readonly eventRouter: (
    source: chrome.debugger.Debuggee,
    method: string,
    params?: object
  ) => void;

  constructor(readonly tabId: number) {
    this.target = { tabId };
    this.eventRouter = (source, method, params) => {
      if (source.tabId !== this.tabId) {
        return;
      }
      const eventParams = (params ?? {}) as Record<string, unknown>;
      for (const listener of this.listeners.get(method) ?? []) {
        listener(eventParams);
      }
    };
  }

  async attach(): Promise<void> {
    try {
      await chrome.debugger.attach(this.target, "1.3");
      this.attached = true;
      chrome.debugger.onEvent.addListener(this.eventRouter);
      await Promise.all([this.send("Page.enable"), this.send("Runtime.enable")]);
    } catch (error) {
      throw new BrowserSearchError(
        "browser_unavailable",
        error instanceof Error ? error.message : String(error),
        true
      );
    }
  }

  async navigate(url: string, timeoutMs: number, signal: AbortSignal): Promise<void> {
    const eventController = new AbortController();
    const abortEventWait = () => eventController.abort();
    signal.addEventListener("abort", abortEventWait, { once: true });
    if (signal.aborted) {
      abortEventWait();
    }
    const loaded = this.waitForEvent(
      "Page.loadEventFired",
      timeoutMs,
      eventController.signal
    );
    void loaded.catch(() => undefined);

    try {
      const result = await this.send<{ errorText?: string }>("Page.navigate", {
        url
      });
      if (result.errorText) {
        throw new BrowserSearchError(
          "navigation_failed",
          result.errorText,
          true
        );
      }
      await loaded;
    } finally {
      eventController.abort();
      signal.removeEventListener("abort", abortEventWait);
    }
  }

  async evaluate<T>(
    expression: string,
    timeoutMs: number,
    signal: AbortSignal
  ): Promise<T> {
    const response = await withAbortAndTimeout(
      this.send<RuntimeResponse>("Runtime.evaluate", {
        expression,
        awaitPromise: true,
        returnByValue: true,
        userGesture: false
      }),
      timeoutMs,
      signal,
      "等待搜索结果超时"
    );
    if (response.exceptionDetails) {
      throw new BrowserSearchError(
        "extraction_failed",
        response.exceptionDetails.exception?.description ??
          response.exceptionDetails.text ??
          "页面抽取脚本执行失败",
        false
      );
    }
    return response.result?.value as T;
  }

  async detach(): Promise<void> {
    if (!this.attached) {
      return;
    }
    this.attached = false;
    chrome.debugger.onEvent.removeListener(this.eventRouter);
    this.listeners.clear();
    await chrome.debugger.detach(this.target).catch(() => undefined);
  }

  private async send<T extends object = Record<string, unknown>>(
    method: string,
    params?: Record<string, unknown>
  ): Promise<T> {
    try {
      return (await chrome.debugger.sendCommand(
        this.target,
        method,
        params
      )) as unknown as T;
    } catch (error) {
      throw new BrowserSearchError(
        "extraction_failed",
        `CDP ${method} failed: ${error instanceof Error ? error.message : String(error)}`,
        true
      );
    }
  }

  private waitForEvent(
    method: string,
    timeoutMs: number,
    signal: AbortSignal
  ): Promise<Record<string, unknown>> {
    return new Promise((resolve, reject) => {
      let settled = false;
      const listeners = this.listeners.get(method) ?? new Set<EventListener>();
      const listener: EventListener = (params) => finish(() => resolve(params));
      const onAbort = () =>
        finish(() =>
          reject(new BrowserSearchError("cancelled", "搜索任务已取消", false))
        );
      const timer = setTimeout(
        () =>
          finish(() =>
            reject(new BrowserSearchError("navigation_failed", "页面加载超时", true))
          ),
        timeoutMs
      );

      const finish = (callback: () => void) => {
        if (settled) {
          return;
        }
        settled = true;
        clearTimeout(timer);
        listeners.delete(listener);
        signal.removeEventListener("abort", onAbort);
        if (listeners.size === 0) {
          this.listeners.delete(method);
        }
        callback();
      };

      listeners.add(listener);
      this.listeners.set(method, listeners);
      signal.addEventListener("abort", onAbort, { once: true });
      if (signal.aborted) {
        onAbort();
      }
    });
  }
}

function withAbortAndTimeout<T>(
  promise: Promise<T>,
  timeoutMs: number,
  signal: AbortSignal,
  timeoutMessage: string
): Promise<T> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const onAbort = () =>
      finish(() =>
        reject(new BrowserSearchError("cancelled", "搜索任务已取消", false))
      );
    const timer = setTimeout(
      () =>
        finish(() =>
          reject(new BrowserSearchError("timeout", timeoutMessage, true))
        ),
      timeoutMs
    );
    const finish = (callback: () => void) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      signal.removeEventListener("abort", onAbort);
      callback();
    };

    signal.addEventListener("abort", onAbort, { once: true });
    if (signal.aborted) {
      onAbort();
      return;
    }
    promise.then(
      (value) => finish(() => resolve(value)),
      (error) => finish(() => reject(error))
    );
  });
}
