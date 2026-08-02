export const PROTOCOL_VERSION = 1;

export type SearchKind = "web" | "news" | "images" | "videos" | "forums";

export interface SearchCommand {
  kind: SearchKind;
  query: string;
  start: number;
  limit: number;
  timeoutMs: number;
  loadTimeoutMs: number;
  selectorTimeoutMs: number;
  url: string;
  extraction: ExtractionRules;
}

export type FieldTransform = "none" | "absolute_url" | "google_url";

export interface ExtractionFieldRule {
  selectors: string[];
  attribute?: string | null;
  transform: FieldTransform;
  required: boolean;
  maxLength?: number | null;
}

export interface ExtractionRules {
  rootSelectors: string[];
  dedupeField: string;
  fields: Record<string, ExtractionFieldRule>;
}

export type SearchResult = Record<string, string>;

export interface SearchResultPayload {
  results: SearchResult[];
}

export type ErrorCode =
  | "unauthorized"
  | "invalid_request"
  | "browser_unavailable"
  | "queue_full"
  | "timeout"
  | "navigation_failed"
  | "extraction_failed"
  | "cancelled"
  | "protocol_error"
  | "instance_conflict"
  | "internal_error";

export interface ErrorPayload {
  code: ErrorCode;
  message: string;
  retryable: boolean;
}

interface Envelope {
  version: number;
  type: string;
  requestId?: string | null;
  payload: unknown;
}

export interface WelcomeMessage extends Envelope {
  type: "welcome";
  payload: {
    protocolVersion: number;
    sessionId: string;
    pingIntervalSeconds: number;
    minOperationIntervalMs: number;
  };
}

export interface SearchMessage extends Envelope {
  type: "search";
  requestId: string;
  payload: SearchCommand;
}

export interface CancelMessage extends Envelope {
  type: "cancel";
  requestId: string;
  payload: { reason?: string };
}

export interface PingMessage extends Envelope {
  type: "ping";
  payload: { nonce?: string | null };
}

export interface PongMessage extends Envelope {
  type: "pong";
  payload: { nonce?: string | null };
}

export interface ServerErrorMessage extends Envelope {
  type: "error";
  requestId?: string | null;
  payload: ErrorPayload;
}

export type IncomingMessage =
  | WelcomeMessage
  | SearchMessage
  | CancelMessage
  | PingMessage
  | PongMessage
  | ServerErrorMessage;

export type ProgressStage =
  | "creating_tab"
  | "navigating"
  | "waiting"
  | "collecting"
  | "closing";

export type OutgoingMessage =
  | {
      version: number;
      type: "hello";
      payload: {
        extensionToken: string;
        browserInstanceId: string;
        browserName: string;
        extensionVersion: string;
        protocolVersion: number;
      };
    }
  | {
      version: number;
      type: "accepted";
      requestId: string;
      payload: Record<string, never>;
    }
  | {
      version: number;
      type: "progress";
      requestId: string;
      payload: { stage: ProgressStage };
    }
  | {
      version: number;
      type: "search_result";
      requestId: string;
      payload: SearchResultPayload;
    }
  | {
      version: number;
      type: "cleanup_complete";
      requestId: string;
      payload: Record<string, never>;
    }
  | {
      version: number;
      type: "error";
      requestId: string;
      payload: ErrorPayload;
    }
  | {
      version: number;
      type: "pong";
      payload: { nonce?: string | null };
    };

export function parseIncomingMessage(raw: string): IncomingMessage {
  const parsed: unknown = JSON.parse(raw);
  if (!isObject(parsed)) {
    throw new Error("Bridge message must be an object");
  }
  if (parsed.version !== PROTOCOL_VERSION || typeof parsed.type !== "string") {
    throw new Error("Unsupported bridge message");
  }
  if (!("payload" in parsed)) {
    throw new Error("Bridge message is missing payload");
  }
  return parsed as unknown as IncomingMessage;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export class BrowserSearchError extends Error {
  constructor(
    readonly code: ErrorCode,
    message: string,
    readonly retryable: boolean
  ) {
    super(message);
    this.name = "BrowserSearchError";
  }

  toPayload(): ErrorPayload {
    return {
      code: this.code,
      message: this.message,
      retryable: this.retryable
    };
  }
}

export function asBrowserSearchError(error: unknown): BrowserSearchError {
  if (error instanceof BrowserSearchError) {
    return error;
  }
  if (error instanceof Error) {
    return new BrowserSearchError("extraction_failed", error.message, false);
  }
  return new BrowserSearchError("extraction_failed", String(error), false);
}
