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
  BucketDeleteMode,
  CreateEventBody,
  CreateEventOptions,
  CreateEventResult,
  CreateMyEventBody,
  DeleteBucketResult,
  DeletedBucketsList,
  EdgeHealth,
  EdgeInfo,
  EventList,
  ListMyBucketEventsOptions,
  ListMyBucketsOptions,
  ListMyEventsOptions,
  MyBucketDetail,
  MyBucketEventsList,
  MyBucketsList,
  MyEventsList,
  Reservation,
} from "./types.js";

export interface ClientOptions {
  /** Override the prod bootstrap list — useful for local clusters. */
  edges?: string[];
  /** Per-request timeout in milliseconds. Default 30s. */
  timeoutMs?: number;
  /** Plug in a custom fetch (e.g. an instrumented wrapper). */
  fetch?: typeof fetch;
  /** @internal Reuse an existing selector — used by `withApiKey`. */
  _selector?: EdgeSelector;
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
    this.selector = opts._selector ?? new EdgeSelector(opts.edges ?? DEFAULT_EDGES);
  }

  /**
   * Clone this client while replacing only the bearer token. The HTTP
   * impl and edge selector are shared, so callers can mint short-lived
   * tokens without losing failover state.
   */
  withApiKey(apiKey: string): Client {
    if (!apiKey || !apiKey.trim()) {
      throw new Error("api_key is required");
    }
    return new Client(apiKey, {
      timeoutMs: this.timeoutMs,
      fetch: this.fetchImpl,
      _selector: this.selector,
    });
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
    if (opts.skipHold !== undefined) {
      body.skip_hold = opts.skipHold;
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

  // ── /v1/me/* (dashboard-namespaced) ─────────────────────────────

  /** List the current user's buckets. */
  async listMyBuckets(opts: ListMyBucketsOptions = {}): Promise<MyBucketsList> {
    return this.request<MyBucketsList>("GET", "/v1/me/buckets", {
      query: { page: opts.page, limit: opts.limit, q: opts.q },
    });
  }

  /** List the current user's tombstoned (deleted) buckets. */
  async listMyDeletedBuckets(): Promise<DeletedBucketsList> {
    return this.request<DeletedBucketsList>("GET", "/v1/me/buckets/deleted");
  }

  /** Get one bucket plus its per-account rollup. */
  async getMyBucket(bucket: string): Promise<MyBucketDetail> {
    return this.request<MyBucketDetail>(
      "GET",
      `/v1/me/buckets/${encodeURIComponent(bucket)}`,
    );
  }

  /** Paginated event list for one of the current user's buckets. */
  async listMyBucketEvents(
    bucket: string,
    opts: ListMyBucketEventsOptions = {},
  ): Promise<MyBucketEventsList> {
    return this.request<MyBucketEventsList>(
      "GET",
      `/v1/me/buckets/${encodeURIComponent(bucket)}/events`,
      {
        query: {
          q: opts.q,
          account: opts.account,
          page: opts.page,
          limit: opts.limit,
        },
      },
    );
  }

  /** Create an event in one of the current user's buckets. */
  async createMyBucketEvent(
    bucket: string,
    body: CreateMyEventBody,
  ): Promise<CreateEventResult> {
    const result = await this.createMyBucketEventWithStatus(bucket, body);
    return result.body;
  }

  /** Same as `createMyBucketEvent` but also returns the HTTP status —
   *  useful for distinguishing 200 (idempotent retry) from 201. */
  async createMyBucketEventWithStatus(
    bucket: string,
    body: CreateMyEventBody,
  ): Promise<{ status: number; body: CreateEventResult }> {
    return this.request<CreateEventResult>(
      "POST",
      `/v1/me/buckets/${encodeURIComponent(bucket)}/events`,
      { body: JSON.stringify(body), returnStatus: true },
    );
  }

  /** Cross-bucket event search across the current user's namespace. */
  async listMyEvents(opts: ListMyEventsOptions = {}): Promise<MyEventsList> {
    return this.request<MyEventsList>("GET", "/v1/me/events", {
      query: {
        bucket: opts.bucket,
        account: opts.account,
        origin: opts.origin,
        event_type: opts.event_type,
        since_ms: opts.since_ms,
        until_ms: opts.until_ms,
        search: opts.search,
        limit: opts.limit,
        offset: opts.offset,
        replication: opts.replication,
      },
    });
  }

  /** Delete one of the current user's buckets. Currently `mode` must
   *  be `"nuke"` — purges the bucket and its events. */
  async deleteMyBucket(
    bucket: string,
    mode: BucketDeleteMode = "nuke",
  ): Promise<DeleteBucketResult> {
    return this.request<DeleteBucketResult>(
      "DELETE",
      `/v1/me/buckets/${encodeURIComponent(bucket)}`,
      { query: { mode } },
    );
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
    method: "GET" | "POST" | "DELETE",
    path: string,
    opts?: {
      body?: string;
      query?: Record<string, string | number | boolean | undefined>;
      returnStatus?: false;
    },
  ): Promise<R>;
  private async request<R>(
    method: "GET" | "POST" | "DELETE",
    path: string,
    opts: {
      body?: string;
      query?: Record<string, string | number | boolean | undefined>;
      returnStatus: true;
    },
  ): Promise<{ status: number; body: R }>;
  private async request<R>(
    method: "GET" | "POST" | "DELETE",
    path: string,
    opts: {
      body?: string;
      query?: Record<string, string | number | boolean | undefined>;
      returnStatus?: boolean;
    } = {},
  ): Promise<R | { status: number; body: R }> {
    await this.ensureProbed();
    let urls = this.selector.liveUrls();
    if (urls.length === 0) {
      await this.selector.probeAll(this.fetchImpl);
      urls = this.selector.liveUrls();
    }
    if (urls.length === 0) {
      throw new ServiceUnavailableError("all edges unhealthy");
    }

    let query = "";
    if (opts.query) {
      const params = new URLSearchParams();
      for (const [k, v] of Object.entries(opts.query)) {
        if (v !== undefined) {
          params.append(k, String(v));
        }
      }
      const qs = params.toString();
      if (qs) query = "?" + qs;
    }

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
          const body = (await resp.json()) as R;
          if (opts.returnStatus) {
            return { status: resp.status, body };
          }
          return body;
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
