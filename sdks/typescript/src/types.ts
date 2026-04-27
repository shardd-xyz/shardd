/** One immutable ledger entry. */
export interface Event {
  event_id: string;
  origin_node_id: string;
  origin_epoch: number;
  origin_seq: number;
  created_at_unix_ms: number;
  type: string;
  bucket: string;
  account: string;
  amount: number;
  note: string | null;
  idempotency_nonce: string;
  void_ref: string | null;
  hold_amount: number;
  hold_expires_at_unix_ms: number;
}

/** Optional knobs for {@link Client.createEvent}. */
export interface CreateEventOptions {
  /** Human-readable description stored on the event. */
  note?: string;
  /**
   * Supply your own dedup key. If omitted, the SDK generates a UUID v4.
   * Reuse the same nonce across retries of the same logical operation —
   * the server returns the original event instead of double-charging.
   */
  idempotencyNonce?: string;
  /** Allow a debit to drive the balance this far negative. Default 0. */
  maxOverdraft?: number;
  /** Wait for at least this many cross-region acks before returning. */
  minAcks?: number;
  /** Cap the ack wait at this many milliseconds. */
  ackTimeoutMs?: number;
  /**
   * Caller-driven reservation amount. Set together with
   * `holdExpiresAtUnixMs` to override the node's default
   * `hold_multiplier × |amount|` sizing on a debit, or — with
   * `amount == 0` — to mint a pure pre-auth reservation. See
   * {@link Client.reserve} for the high-level flow.
   */
  holdAmount?: number;
  /** Unix-ms timestamp at which the hold auto-releases. */
  holdExpiresAtUnixMs?: number;
  /**
   * One-shot capture against an existing reservation. Pair with a
   * negative `amount` ≤ the reservation's hold amount; the server
   * emits both the charge and a `hold_release` atomically, returning
   * any unused remainder to available balance.
   */
  settleReservation?: string;
  /** Cancel a reservation outright. Pair with `amount: 0`. */
  releaseReservation?: string;
}

/** A reservation handle returned by {@link Client.reserve}. */
export interface Reservation {
  reservationId: string;
  expiresAtUnixMs: number;
  balance: number;
  availableBalance: number;
}

export interface CreateEventResult {
  event: Event;
  /**
   * Every event minted by this request. For a settle, this contains
   * both the Standard charge and the matching `hold_release`. For a
   * debit that triggered the implicit hold, contains the
   * `reservation_create` and the charge. Empty on an idempotent retry.
   */
  emitted_events?: Event[];
  balance: number;
  available_balance: number;
  /** `true` on an idempotent retry — the write was a no-op. */
  deduplicated: boolean;
  acks: AckInfo;
}

export interface AckInfo {
  requested: number;
  received: number;
  timeout: boolean;
}

export interface EventList {
  events: Event[];
}

export interface AccountBalance {
  bucket: string;
  account: string;
  balance: number;
  available_balance: number;
  active_hold_total: number;
  event_count: number;
}

export interface Balances {
  accounts: AccountBalance[];
  total_balance: number;
}

export type AccountDetail = AccountBalance;

export interface EdgeInfo {
  edge_id: string;
  region: string;
  base_url: string;
  ready: boolean;
  reachable: boolean;
  sync_gap: number | null;
  overloaded: boolean | null;
  healthy_nodes: number;
  discovered_nodes: number;
  best_node_rtt_ms: number | null;
}

export interface EdgeHealth {
  edge_id: string | null;
  region: string | null;
  base_url: string | null;
  ready: boolean;
  discovered_nodes: number;
  healthy_nodes: number;
  best_node_rtt_ms: number | null;
  sync_gap: number | null;
  overloaded: boolean | null;
  auth_enabled: boolean;
}

/** Internal request-body shape — snake_case on the wire. */
export interface CreateEventBody {
  bucket: string;
  account: string;
  amount: number;
  idempotency_nonce: string;
  note?: string;
  max_overdraft?: number;
  min_acks?: number;
  ack_timeout_ms?: number;
  hold_amount?: number;
  hold_expires_at_unix_ms?: number;
  settle_reservation?: string;
  release_reservation?: string;
}
