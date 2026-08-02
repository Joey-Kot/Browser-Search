import type { BridgeClient } from "./bridge";
import { CdpSession } from "./cdp";
import { cleanupJob } from "./cleanup";
import { buildGoogleExtractionExpression } from "./engines/google";
import {
  listPersistedJobs,
  persistJob,
  removePersistedJob
} from "./storage";
import {
  PROTOCOL_VERSION,
  asBrowserSearchError,
  BrowserSearchError,
  type IncomingMessage,
  type ProgressStage,
  type SearchCommand,
  type SearchResult,
  type SearchResultPayload
} from "./types";

interface JobContext {
  requestId: string;
  command: SearchCommand;
  createdAt: number;
  tabId: number | null;
  cdp: CdpSession | null;
  abortController: AbortController;
  cancelledByDaemon: boolean;
  cleanupBlocked: boolean;
  completion: Promise<void> | null;
}

interface OpeningWaiter {
  signal: AbortSignal;
  resolve: (release: () => void) => void;
  reject: (error: BrowserSearchError) => void;
  onAbort: () => void;
}

export class JobManager {
  private readonly jobs = new Map<string, JobContext>();
  private operationIntervalMs = 500;
  private operationNotBefore = 0;
  private cleanupBlockers = 0;
  private openingLocked = false;
  private readonly openingWaiters: OpeningWaiter[] = [];
  private readonly gateWaiters = new Set<() => void>();

  constructor(private readonly bridge: BridgeClient) {}

  async initialize(): Promise<void> {
    const persisted = await listPersistedJobs();
    for (const job of persisted) {
      await chrome.tabs.remove(job.tabId).catch(() => undefined);
      await removePersistedJob(job.requestId);
    }
  }

  async handleMessage(message: IncomingMessage): Promise<void> {
    switch (message.type) {
      case "search":
        this.startJob(message.requestId, message.payload);
        break;
      case "cancel":
        await this.cancelJob(message.requestId);
        break;
      case "error":
        if (message.requestId) {
          await this.cancelJob(message.requestId);
        }
        break;
      case "welcome":
        this.operationIntervalMs = normalizeOperationInterval(
          message.payload.minOperationIntervalMs
        );
        break;
      case "ping":
      case "pong":
        break;
    }
  }

  async handleDisconnect(): Promise<void> {
    const contexts = [...this.jobs.values()];
    for (const context of contexts) {
      this.blockForCleanup(context);
      context.cancelledByDaemon = true;
      context.abortController.abort();
    }
    await Promise.allSettled(
      contexts.flatMap((context) =>
        context.completion === null ? [] : [context.completion]
      )
    );
  }

  private startJob(requestId: string, command: SearchCommand): void {
    if (this.jobs.has(requestId)) {
      this.sendAccepted(requestId);
      return;
    }
    const context: JobContext = {
      requestId,
      command,
      createdAt: Date.now(),
      tabId: null,
      cdp: null,
      abortController: new AbortController(),
      cancelledByDaemon: false,
      cleanupBlocked: false,
      completion: null
    };
    this.jobs.set(requestId, context);
    this.sendAccepted(requestId);
    context.completion = this.runJob(context);
    void context.completion.catch(() => undefined);
  }

  private async runJob(context: JobContext): Promise<void> {
    let result: SearchResultPayload | null = null;
    let failure: BrowserSearchError | null = null;
    try {
      await this.progress(context, "creating_tab");
      ensureActive(context);
      const tab = await this.createTaskTab(context);
      if (tab.id === undefined) {
        throw new BrowserSearchError(
          "browser_unavailable",
          "Chrome returned no tab id",
          true
        );
      }
      context.tabId = tab.id;
      await persistJob({
        requestId: context.requestId,
        tabId: tab.id,
        createdAt: context.createdAt
      });
      ensureActive(context);

      context.cdp = new CdpSession(tab.id);
      await context.cdp.attach();
      ensureActive(context);
      await this.progress(context, "navigating");
      await context.cdp.navigate(
        context.command.url,
        remaining(context, context.command.loadTimeoutMs),
        context.abortController.signal
      );

      await this.progress(context, "waiting");
      const selectorWaitMs = remaining(
        context,
        context.command.selectorTimeoutMs
      );
      const expression = buildGoogleExtractionExpression(
        context.command.extraction,
        context.command.limit,
        selectorWaitMs,
        context.command.kind === "images"
      );
      await this.progress(context, "collecting");
      result = await context.cdp.evaluate<SearchResultPayload>(
        expression,
        Math.min(remaining(context), selectorWaitMs + 2_000),
        context.abortController.signal
      );
      if (!result || !Array.isArray(result.results)) {
        throw new BrowserSearchError(
          "protocol_error",
          "页面抽取脚本没有返回结果数组",
          false
        );
      }
      result.results = result.results
        .filter(isSearchResult)
        .slice(0, context.command.limit);
    } catch (error) {
      failure = asBrowserSearchError(error);
    } finally {
      const hadTaskTab = context.tabId !== null;
      await this.progress(context, "closing").catch(() => undefined);
      await cleanupJob({
        requestId: context.requestId,
        tabId: context.tabId,
        cdp: context.cdp,
        abortController: context.abortController
      }).catch(() => undefined);
      this.finishCleanup(context, hadTaskTab);
      if (this.jobs.get(context.requestId) === context) {
        this.jobs.delete(context.requestId);
      }
    }

    if (context.cancelledByDaemon) {
      if (this.bridge.isConnected()) {
        try {
          this.bridge.send({
            version: PROTOCOL_VERSION,
            type: "cleanup_complete",
            requestId: context.requestId,
            payload: {}
          });
        } catch {
          // The reconnect path confirms cleanup after all disconnected jobs finish.
        }
      }
      return;
    }
    if (!this.bridge.isConnected()) {
      return;
    }
    if (failure) {
      this.bridge.send({
        version: PROTOCOL_VERSION,
        type: "error",
        requestId: context.requestId,
        payload: failure.toPayload()
      });
      return;
    }
    this.bridge.send({
      version: PROTOCOL_VERSION,
      type: "search_result",
      requestId: context.requestId,
      payload: result ?? { results: [] }
    });
  }

  private async cancelJob(requestId: string): Promise<void> {
    const context = this.jobs.get(requestId);
    if (!context) {
      return;
    }
    this.blockForCleanup(context);
    context.cancelledByDaemon = true;
    context.abortController.abort();
    if (context.completion !== null) {
      await context.completion;
    }
  }

  private async createTaskTab(context: JobContext): Promise<chrome.tabs.Tab> {
    const signal = context.abortController.signal;
    const release = await this.acquireOpeningLock(signal);
    try {
      while (true) {
        ensureActive(context);
        if (this.cleanupBlockers > 0) {
          await this.waitForGateChange(signal);
          continue;
        }
        const waitMs = this.operationNotBefore - Date.now();
        if (waitMs > 0) {
          await waitWithAbort(waitMs, signal);
          continue;
        }
        ensureActive(context);
        const tab = await chrome.tabs.create({ url: "about:blank", active: false });
        this.extendOperationDeadline();
        return tab;
      }
    } finally {
      release();
    }
  }

  private blockForCleanup(context: JobContext): void {
    if (context.cleanupBlocked) {
      return;
    }
    context.cleanupBlocked = true;
    this.cleanupBlockers += 1;
  }

  private finishCleanup(context: JobContext, hadTaskTab: boolean): void {
    if (hadTaskTab) {
      this.extendOperationDeadline();
    }
    if (!context.cleanupBlocked) {
      return;
    }
    context.cleanupBlocked = false;
    this.cleanupBlockers = Math.max(0, this.cleanupBlockers - 1);
    if (this.cleanupBlockers === 0) {
      const waiters = [...this.gateWaiters];
      this.gateWaiters.clear();
      for (const wake of waiters) {
        wake();
      }
    }
  }

  private extendOperationDeadline(): void {
    this.operationNotBefore = Math.max(
      this.operationNotBefore,
      Date.now() + this.operationIntervalMs
    );
  }

  private waitForGateChange(signal: AbortSignal): Promise<void> {
    return new Promise((resolve, reject) => {
      let settled = false;
      const finish = (callback: () => void) => {
        if (settled) {
          return;
        }
        settled = true;
        this.gateWaiters.delete(wake);
        signal.removeEventListener("abort", onAbort);
        callback();
      };
      const wake = () => finish(resolve);
      const onAbort = () => finish(() => reject(cancelledError()));

      this.gateWaiters.add(wake);
      signal.addEventListener("abort", onAbort, { once: true });
      if (signal.aborted) {
        onAbort();
      }
    });
  }

  private acquireOpeningLock(signal: AbortSignal): Promise<() => void> {
    if (signal.aborted) {
      return Promise.reject(cancelledError());
    }
    if (!this.openingLocked) {
      this.openingLocked = true;
      return Promise.resolve(this.openingRelease());
    }

    return new Promise((resolve, reject) => {
      let waiter!: OpeningWaiter;
      const onAbort = () => {
        const index = this.openingWaiters.indexOf(waiter);
        if (index >= 0) {
          this.openingWaiters.splice(index, 1);
        }
        signal.removeEventListener("abort", onAbort);
        reject(cancelledError());
      };
      waiter = { signal, resolve, reject, onAbort };
      this.openingWaiters.push(waiter);
      signal.addEventListener("abort", onAbort, { once: true });
      if (signal.aborted) {
        onAbort();
      }
    });
  }

  private openingRelease(): () => void {
    let released = false;
    return () => {
      if (released) {
        return;
      }
      released = true;
      while (this.openingWaiters.length > 0) {
        const waiter = this.openingWaiters.shift();
        if (!waiter) {
          break;
        }
        waiter.signal.removeEventListener("abort", waiter.onAbort);
        if (waiter.signal.aborted) {
          waiter.reject(cancelledError());
          continue;
        }
        waiter.resolve(this.openingRelease());
        return;
      }
      this.openingLocked = false;
    };
  }

  private sendAccepted(requestId: string): void {
    if (!this.bridge.isConnected()) {
      return;
    }
    this.bridge.send({
      version: PROTOCOL_VERSION,
      type: "accepted",
      requestId,
      payload: {}
    });
  }

  private async progress(
    context: JobContext,
    stage: ProgressStage
  ): Promise<void> {
    if (!this.bridge.isConnected()) {
      return;
    }
    this.bridge.send({
      version: PROTOCOL_VERSION,
      type: "progress",
      requestId: context.requestId,
      payload: { stage }
    });
  }
}

function normalizeOperationInterval(value: number): number {
  if (!Number.isSafeInteger(value) || value < 0) {
    return 0;
  }
  return value;
}

function cancelledError(): BrowserSearchError {
  return new BrowserSearchError("cancelled", "搜索任务已取消", false);
}

function waitWithAbort(milliseconds: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const timer = setTimeout(() => finish(resolve), milliseconds);
    const onAbort = () => finish(() => reject(cancelledError()));
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
    }
  });
}

function ensureActive(context: JobContext): void {
  if (context.abortController.signal.aborted) {
    throw cancelledError();
  }
}

function remaining(context: JobContext, maximum = Number.POSITIVE_INFINITY): number {
  const milliseconds = context.createdAt + context.command.timeoutMs - Date.now();
  if (milliseconds <= 0) {
    throw new BrowserSearchError("timeout", "搜索任务超时", true);
  }
  return Math.max(1, Math.min(milliseconds, maximum));
}

function isSearchResult(value: unknown): value is SearchResult {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return false;
  }
  const entries = Object.entries(value);
  return (
    entries.length > 0 &&
    entries.every(
      ([name, fieldValue]) => name.length > 0 && typeof fieldValue === "string"
    )
  );
}
