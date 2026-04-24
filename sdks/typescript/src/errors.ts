/** Base class for every error the SDK surfaces. Callers can branch on
 *  `err.code` or use `instanceof` against the specific subclasses. */
export class ShardError extends Error {
  public readonly code: ShardErrorCode;
  constructor(code: ShardErrorCode, message: string) {
    super(message);
    this.name = "ShardError";
    this.code = code;
  }
  /** `true` for errors where a retry might succeed. */
  get retryable(): boolean {
    return (
      this.code === "service_unavailable" ||
      this.code === "timeout" ||
      this.code === "network"
    );
  }
}

export type ShardErrorCode =
  | "invalid_input"
  | "unauthorized"
  | "forbidden"
  | "not_found"
  | "insufficient_funds"
  | "payment_required"
  | "service_unavailable"
  | "timeout"
  | "network"
  | "decode";

export class InvalidInputError extends ShardError {
  constructor(message: string) {
    super("invalid_input", message);
    this.name = "InvalidInputError";
  }
}

export class UnauthorizedError extends ShardError {
  constructor(message: string) {
    super("unauthorized", message);
    this.name = "UnauthorizedError";
  }
}

export class ForbiddenError extends ShardError {
  constructor(message: string) {
    super("forbidden", message);
    this.name = "ForbiddenError";
  }
}

export class NotFoundError extends ShardError {
  constructor(message: string) {
    super("not_found", message);
    this.name = "NotFoundError";
  }
}

export class InsufficientFundsError extends ShardError {
  public readonly balance: number;
  public readonly availableBalance: number;
  public readonly limit: number;
  constructor(balance: number, availableBalance: number, limit: number) {
    super(
      "insufficient_funds",
      `insufficient funds: balance=${balance}, available=${availableBalance}`,
    );
    this.name = "InsufficientFundsError";
    this.balance = balance;
    this.availableBalance = availableBalance;
    this.limit = limit;
  }
}

export class PaymentRequiredError extends ShardError {
  constructor() {
    super("payment_required", "payment required");
    this.name = "PaymentRequiredError";
  }
}

export class ServiceUnavailableError extends ShardError {
  constructor(message: string) {
    super("service_unavailable", message);
    this.name = "ServiceUnavailableError";
  }
}

export class TimeoutError extends ShardError {
  constructor() {
    super("timeout", "request timed out");
    this.name = "TimeoutError";
  }
}

export class NetworkError extends ShardError {
  constructor(message: string) {
    super("network", message);
    this.name = "NetworkError";
  }
}

export class DecodeError extends ShardError {
  constructor(message: string) {
    super("decode", message);
    this.name = "DecodeError";
  }
}

interface GatewayErrorBody {
  error?: string;
  message?: string;
  balance?: number;
  available_balance?: number;
  limit?: number;
}

export function fromStatus(status: number, body?: GatewayErrorBody): ShardError {
  const text = body?.error ?? body?.message ?? `HTTP ${status}`;
  switch (status) {
    case 400:
      return new InvalidInputError(text);
    case 401:
      return new UnauthorizedError(text);
    case 402:
      return new PaymentRequiredError();
    case 403:
      return new ForbiddenError(text);
    case 404:
      return new NotFoundError(text);
    case 422:
      return new InsufficientFundsError(
        body?.balance ?? 0,
        body?.available_balance ?? 0,
        body?.limit ?? 0,
      );
    case 408:
    case 504:
      return new TimeoutError();
    case 503:
      return new ServiceUnavailableError(text);
    default:
      return new DecodeError(`unexpected HTTP ${status}: ${text}`);
  }
}
