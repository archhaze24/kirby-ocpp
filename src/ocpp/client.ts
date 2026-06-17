import EventEmitter from "node:events";
import { nanoid } from "nanoid";
import WebSocket from "ws";
import type {
  CallErrorResponse,
  CallResponse,
  OcppCall,
  OcppCallError,
  OcppCallResult,
  OcppMessage,
  PendingCall
} from "./types.js";
import { OcppSchemaValidator } from "./schema-validator.js";

type OcppCallErrorCode =
  | "NotImplemented"
  | "NotSupported"
  | "InternalError"
  | "ProtocolError"
  | "SecurityError"
  | "FormationViolation"
  | "PropertyConstraintViolation"
  | "OccurenceConstraintViolation"
  | "TypeConstraintViolation"
  | "GenericError";

interface OcppClientEvents {
  connected: [];
  disconnected: [code?: number, reason?: string];
  error: [error: Error];
  log: [direction: "in" | "out", message: OcppMessage];
  call: [messageId: string, action: string, payload: Record<string, unknown>];
}

export declare interface OcppClient {
  on<K extends keyof OcppClientEvents>(event: K, listener: (...args: OcppClientEvents[K]) => void): this;
  emit<K extends keyof OcppClientEvents>(event: K, ...args: OcppClientEvents[K]): boolean;
}

export class OcppClient extends EventEmitter {
  private socket?: WebSocket;
  private readonly pending = new Map<string, PendingCall>();
  private readonly incomingCalls = new Map<string, string>();
  private readonly schemaValidator = new OcppSchemaValidator();

  constructor(
    private readonly centralSystemUrl: string,
    private readonly chargePointId: string
  ) {
    super();
  }

  get isConnected(): boolean {
    return this.socket?.readyState === WebSocket.OPEN;
  }

  connect(): void {
    this.close();

    const url = this.buildChargePointUrl();
    this.socket = new WebSocket(url, ["ocpp1.6"]);

    this.socket.on("open", () => this.emit("connected"));
    this.socket.on("message", (data) => this.handleMessage(data.toString()));
    this.socket.on("close", (code, reason) => {
      this.rejectPending(new Error(`Connection closed (${code})`));
      this.emit("disconnected", code, reason.toString());
    });
    this.socket.on("error", (error) => this.emit("error", error));
  }

  close(): void {
    if (!this.socket) {
      return;
    }

    this.rejectPending(new Error("Connection closed"));
    this.socket.removeAllListeners();
    if (this.socket.readyState === WebSocket.OPEN || this.socket.readyState === WebSocket.CONNECTING) {
      this.socket.close();
    }
    this.socket = undefined;
  }

  async call(action: string, payload: Record<string, unknown>, timeoutMs = 30_000): Promise<CallResponse> {
    if (!this.isConnected || !this.socket) {
      throw new Error("Not connected to Central System");
    }

    this.assertValidRequest(action, payload);

    const messageId = nanoid(12);
    const message: OcppCall = [2, messageId, action, payload];

    const response = new Promise<CallResponse>((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(messageId);
        reject(new Error(`${action} timed out after ${timeoutMs}ms`));
      }, timeoutMs);

      this.pending.set(messageId, { action, resolve, reject, timeout });
    });

    this.send(message);
    return response;
  }

  reply(messageId: string, payload: Record<string, unknown>): void {
    const action = this.incomingCalls.get(messageId);
    if (!action) {
      throw new Error(`Cannot reply to unknown CALL ${messageId}`);
    }

    this.assertValidResponse(action, payload);
    this.incomingCalls.delete(messageId);
    this.send([3, messageId, payload]);
  }

  replyError(
    messageId: string,
    code: string,
    description: string,
    details: Record<string, unknown> = {}
  ): void {
    this.incomingCalls.delete(messageId);
    this.send([4, messageId, code, description, details]);
  }

  private send(message: OcppMessage): void {
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN) {
      throw new Error("WebSocket is not open");
    }

    this.socket.send(JSON.stringify(message));
    this.emit("log", "out", message);
  }

  private handleMessage(raw: string): void {
    let message: OcppMessage;

    try {
      message = JSON.parse(raw) as OcppMessage;
    } catch (error) {
      this.emit("error", error instanceof Error ? error : new Error(String(error)));
      return;
    }

    this.emit("log", "in", message);

    if (!Array.isArray(message)) {
      this.emit("error", new Error("Invalid OCPP message: expected array"));
      return;
    }

    if (message[0] === 2) {
      if (!this.isValidCall(message)) {
        const messageId = this.readMessageId(message);
        if (messageId) {
          this.replyError(messageId, "FormationViolation", "Invalid CALL frame");
        }
        this.emit("error", new Error("Invalid CALL frame"));
        return;
      }

      const call = message as OcppCall;
      if (!this.schemaValidator.hasRequestSchema(call[2])) {
        this.replyError(call[1], "NotImplemented", `${call[2]} is not an OCPP 1.6J action`);
        return;
      }

      const validation = this.schemaValidator.validateRequest(call[2], call[3]);
      if (!validation.valid) {
        this.replyError(call[1], this.validationErrorCode(validation.errorNames), validation.errors.join("; "));
        return;
      }

      this.incomingCalls.set(call[1], call[2]);
      this.emit("call", call[1], call[2], call[3] ?? {});
      return;
    }

    if (message[0] === 3) {
      if (!this.isValidCallResult(message)) {
        this.emit("error", new Error("Invalid CALLRESULT frame"));
        return;
      }

      this.resolveCall(message as OcppCallResult);
      return;
    }

    if (message[0] === 4) {
      if (!this.isValidCallError(message)) {
        this.emit("error", new Error("Invalid CALLERROR frame"));
        return;
      }

      this.rejectCall(message as OcppCallError);
      return;
    }

    this.emit("error", new Error(`Unsupported OCPP message type: ${message[0]}`));
  }

  private resolveCall(message: OcppCallResult): void {
    const [, messageId, payload] = message;
    const pending = this.pending.get(messageId);
    if (!pending) {
      return;
    }

    clearTimeout(pending.timeout);
    this.pending.delete(messageId);

    const validation = this.schemaValidator.validateResponse(pending.action, payload);
    if (!validation.valid) {
      pending.reject(
        new Error(
          `${pending.action} response violates OCPP 1.6J schema (${this.validationErrorCode(validation.errorNames)}): ${validation.errors.join("; ")}`
        )
      );
      return;
    }

    pending.resolve({ messageId, payload });
  }

  private rejectCall(message: OcppCallError): void {
    const error = this.toError(message);
    const pending = this.pending.get(error.messageId);
    if (!pending) {
      this.emit("error", new Error(error.description || error.code));
      return;
    }

    clearTimeout(pending.timeout);
    this.pending.delete(error.messageId);
    pending.reject(new Error(`${pending.action} failed: ${error.code} ${error.description}`.trim()));
  }

  private rejectPending(error: Error): void {
    for (const [messageId, pending] of this.pending) {
      clearTimeout(pending.timeout);
      pending.reject(error);
      this.pending.delete(messageId);
    }
  }

  private toError(message: OcppCallError): CallErrorResponse {
    const [, messageId, code, description, details] = message;
    return { messageId, code, description, details };
  }

  private assertValidRequest(action: string, payload: unknown): void {
    const validation = this.schemaValidator.validateRequest(action, payload);
    if (!validation.valid) {
      throw new Error(
        `${action} request violates OCPP 1.6J schema (${this.validationErrorCode(validation.errorNames)}): ${validation.errors.join("; ")}`
      );
    }
  }

  private assertValidResponse(action: string, payload: unknown): void {
    const validation = this.schemaValidator.validateResponse(action, payload);
    if (!validation.valid) {
      throw new Error(
        `${action} response violates OCPP 1.6J schema (${this.validationErrorCode(validation.errorNames)}): ${validation.errors.join("; ")}`
      );
    }
  }

  private validationErrorCode(errorNames: string[]): OcppCallErrorCode {
    if (errorNames.includes("required")) {
      return "OccurenceConstraintViolation";
    }

    if (errorNames.includes("type")) {
      return "TypeConstraintViolation";
    }

    if (
      errorNames.some((name) =>
        ["additionalProperties", "enum", "maxLength", "minLength", "minimum", "maximum", "multipleOf", "format"].includes(name)
      )
    ) {
      return "PropertyConstraintViolation";
    }

    return "FormationViolation";
  }

  private isValidCall(message: unknown[]): boolean {
    return (
      message.length === 4 &&
      message[0] === 2 &&
      typeof message[1] === "string" &&
      typeof message[2] === "string" &&
      this.isObjectPayload(message[3])
    );
  }

  private isValidCallResult(message: unknown[]): boolean {
    return message.length === 3 && message[0] === 3 && typeof message[1] === "string" && this.isObjectPayload(message[2]);
  }

  private isValidCallError(message: unknown[]): boolean {
    return (
      message.length === 5 &&
      message[0] === 4 &&
      typeof message[1] === "string" &&
      typeof message[2] === "string" &&
      typeof message[3] === "string" &&
      this.isObjectPayload(message[4])
    );
  }

  private isObjectPayload(value: unknown): value is Record<string, unknown> {
    return Boolean(value) && typeof value === "object" && !Array.isArray(value);
  }

  private readMessageId(message: unknown[]): string | undefined {
    return typeof message[1] === "string" ? message[1] : undefined;
  }

  private buildChargePointUrl(): string {
    const base = this.centralSystemUrl.replace(/\/+$/, "");
    const id = encodeURIComponent(this.chargePointId);

    if (base.endsWith(`/${id}`)) {
      return base;
    }

    return `${base}/${id}`;
  }
}
