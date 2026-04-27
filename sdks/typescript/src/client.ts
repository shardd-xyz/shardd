import { v4 as uuidv4 } from "uuid";

import {
  DEFAULT_EDGES,
  EdgeSelector,
  fetchDirectory,
  trimTrailingSlash,
} from "./edges.js";
import {
  fromStatus,
  NetworkError,
  ServiceUnavailableError,
  ShardError,
  TimeoutError,
} from "./errors.js";
import {
  AccountDetail,
  Balances,
  CreateEventBody,
  CreateEventOptions,
  CreateEventResult,
  EdgeHealth,
  EdgeInfo,
  EventList,
  Reservation,
} from "./types.js";

export interface ClientOptions {
  /** Override the prod bootstrap list — useful for local clusters. */
  edges?: string[];
  /** Per-request timeout in milliseconds. Default 30s. */
  timeoutMs?: number;
  /** Plug in a custom fetch (e.g. an instrumented wrapper). */
  fetch?: typeof fetch;
}

/** Thread-safe handle to the shardd API.
 *
 * ```ts
 * const shardd = new Client("sk_live_...");
 * const result = await shardd.createEvent("my-app", "user:42", -100);
 * console.log("balance", result.balance);
 * ```
 */
export class Client {
  private readonly apiKey: string;
  private readonly timeoutMs: number;
  private readonly fetchImpl: typeof fetch;
  private readonly selector: EdgeSelector;

  constructor(apiKey: string, opts: ClientOptions = {}) {
    if (!apiKey || !apiKey.trim()) {
      throw new Error("api_key is required");
    }
    this.apiKey = apiKey;
    this.timeoutMs = opts.timeoutMs ?? 30_000;
    this.fetchImpl = opts.fetch ?? fetch.bind(globalThis);
    this.selector = new EdgeSelector(opts.edges ?? DEFAULT_EDGES);
  }

  /**
   * Create a ledger event. Positive `amount` = credit, negative = debit.
   * Auto-generates an `idempotencyNonce` if you don't supply one.
   */
  async createEvent(
    bucket: string,
    account: string,
    amount: number,
    opts: CreateEventOptions = {},
  ): Promise<CreateEventResult> {
    const nonce = opts.idempotencyNonce ?? uuidv4();
    const body: CreateEventBody = {
      bucket,
      account,
      amount,
      idempotency_nonce: nonce,
    };
    if (opts.note !== undefined) body.note = opts.note;
    if (opts.maxOverdraft !== undefined) body.max_overdraft = opts.maxOverdraft;
    if (opts.minAcks !== undefined) body.min_acks = opts.minAcks;
    if (opts.ackTimeoutMs !== undefined) body.ack_timeout_ms = opts.ackTimeoutMs;
    if (opts.holdAmount !== undefined) body.hold_amount = opts.holdAmount;
    if (opts.holdExpiresAtUnixMs !== undefined) {
      body.hold_expires_at_unix_ms = opts.holdExpiresAtUnixMs;
    }
    if (opts.settleReservation !== undefined) {
      body.settle_reservation = opts.settleReservation;
    }
    if (opts.releaseReservation !== undefined) {
      body.release_reservation = opts.releaseReservation;
    }
    return this.request<CreateEventResult>("POST", "/events", {
      body: JSON.stringify(body),
    });
  }

  /** Charge (debit) sugar — absolute value, returns just the event. */
  async charge(
    bucket: string,
    account: string,
    amount: number,
    opts: CreateEventOptions = {},
  ): Promise<CreateEventResult> {
    return this.createEvent(bucket, account, -Math.abs(amount), opts);
  }

  /** Credit sugar — returns the event. */
  async credit(
    bucket: string,
    account: string,
    amount: number,
    opts: CreateEventOptions = {},
  ): Promise<CreateEventResult> {
    return this.createEvent(bucket, account, Math.abs(amount), opts);
  }

  /**
   * Reserve `amount` units against `account` for `ttlMs` milliseconds.
   * Returns a {@link Reservation} handle whose `reservationId` you pass
   * to {@link Client.settle} (one-shot capture) or {@link Client.release}
   * (cancel). If neither is called before `ttlMs` elapses, the hold
   * auto-releases and `availableBalance` recovers.
   */
  async reserve(
    bucket: string,
    account: string,
    amount: number,
    ttlMs: number,
    opts: CreateEventOptions = {},
  ): Promise<Reservation> {
    if (amount <= 0) {
      throw new Error("reserve amount must be > 0");
    }
    if (ttlMs <= 0) {
      throw new Error("reserve ttlMs must be > 0");
    }
    const expiresAtUnixMs = Date.now() + ttlMs;
    const result = await this.createEvent(bucket, account, 0, {
      ...opts,
      holdAmount: amount,
      holdExpiresAtUnixMs: expiresAtUnixMs,
    });
    return {
      reservationId: result.event.event_id,
      expiresAtUnixMs: result.event.hold_expires_at_unix_ms,
      balance: result.balance,
      availableBalance: result.available_balance,
    };
  }

  /**
   * Settle (one-shot capture) `amount` against an existing reservation.
   * `amount` is the absolute value to charge; must be ≤ the reservation's
   * hold. The server emits both the charge and a `hold_release`,
   * returning any unused remainder to available balance.
   */
  async settle(
    bucket: string,
    account: string,
    reservationId: string,
    amount: number,
    opts: CreateEventOptions = {},
  ): Promise<CreateEventResult> {
    return this.createEvent(bucket, account, -Math.abs(amount), {
      ...opts,
      settleReservation: reservationId,
    });
  }

  /** Cancel a reservation outright — releases the entire hold, no charge. */
  async release(
    bucket: string,
    account: string,
    reservationId: string,
    opts: CreateEventOptions = {},
  ): Promise<CreateEventResult> {
    return this.createEvent(bucket, account, 0, {
      ...opts,
      releaseReservation: reservationId,
    });
  }

  async listEvents(bucket: string): Promise<EventList> {
    return this.request<EventList>("GET", "/events", {
      query: { bucket },
    });
  }

  async getBalances(bucket: string): Promise<Balances> {
    return this.request<Balances>("GET", "/balances", {
      query: { bucket },
    });
  }

  async getAccount(bucket: string, account: string): Promise<AccountDetail> {
    const path = `/collapsed/${encodeURIComponent(bucket)}/${encodeURIComponent(
      account,
    )}`;
    return this.request<AccountDetail>("GET", path);
  }

  /** Refresh and return the list of regional edges from the gateway. */
  async edges(): Promise<EdgeInfo[]> {
    await this.ensureProbed();
    const live = this.selector.liveUrls();
    if (live.length === 0) {
      throw new ServiceUnavailableError("no healthy edges");
    }
    const dir = await fetchDirectory(this.fetchImpl, live[0]!);
    return dir.edges;
  }

  /** Health of a specific edge, or the currently-pinned one. */
  async health(baseUrl?: string): Promise<EdgeHealth> {
    const target = baseUrl ?? (await this.pickEdge());
    const resp = await this.fetchImpl(
      `${trimTrailingSlash(target)}/gateway/health`,
    );
    if (!resp.ok) throw fromStatus(resp.status);
    return (await resp.json()) as EdgeHealth;
  }

  // ── internal ────────────────────────────────────────────────────

  private async ensureProbed(): Promise<void> {
    if (this.selector.needsProbe()) {
      await this.selector.probeAll(this.fetchImpl);
    }
  }

  private async pickEdge(): Promise<string> {
    await this.ensureProbed();
    const live = this.selector.liveUrls();
    if (live.length === 0) {
      throw new ServiceUnavailableError("no healthy edges");
    }
    return live[0]!;
  }

  private async request<R>(
    method: "GET" | "POST",
    path: string,
    opts: { body?: string; query?: Record<string, string> } = {},
  ): Promise<R> {
    await this.ensureProbed();
    let urls = this.selector.liveUrls();
    if (urls.length === 0) {
      await this.selector.probeAll(this.fetchImpl);
      urls = this.selector.liveUrls();
    }
    if (urls.length === 0) {
      throw new ServiceUnavailableError("all edges unhealthy");
    }

    const query = opts.query
      ? "?" +
        new URLSearchParams(opts.query as Record<string, string>).toString()
      : "";

    // Try candidates in priority order, capped at 3 (matches our
    // current prod topology). Prevents a request from fanning out to
    // an arbitrary number of edges if the rollout grows.
    let lastErr: ShardError | null = null;
    for (const base of urls.slice(0, 3)) {
      const url = `${trimTrailingSlash(base)}${path}${query}`;
      const ac = new AbortController();
      const timer = setTimeout(() => ac.abort(), this.timeoutMs);
      try {
        const resp = await this.fetchImpl(url, {
          method,
          headers: {
            Authorization: `Bearer ${this.apiKey}`,
            "Content-Type": "application/json",
          },
          body: opts.body,
          signal: ac.signal,
        });
        clearTimeout(timer);
        if (resp.ok) {
          this.selector.markSuccess(base);
          return (await resp.json()) as R;
        }
        const text = await resp.text();
        let body: unknown = undefined;
        try {
          body = text ? JSON.parse(text) : undefined;
        } catch {
          // ignore parse errors
        }
        const err = fromStatus(resp.status, body as {
          error?: string;
          message?: string;
          balance?: number;
          available_balance?: number;
          limit?: number;
        });
        if (!err.retryable) throw err;
        this.selector.markFailure(base);
        lastErr = err;
      } catch (e) {
        clearTimeout(timer);
        if (e instanceof ShardError) {
          if (!e.retryable) throw e;
          this.selector.markFailure(base);
          lastErr = e;
          continue;
        }
        if ((e as Error).name === "AbortError") {
          this.selector.markFailure(base);
          lastErr = new TimeoutError();
          continue;
        }
        this.selector.markFailure(base);
        lastErr = new NetworkError((e as Error).message);
      }
    }
    throw (
      lastErr ??
      new ServiceUnavailableError("failover exhausted with no error captured")
    );
  }
}
