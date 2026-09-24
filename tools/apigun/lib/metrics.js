// Custom metrics, constructed once in the init context.
//
// k6's built-in http_req_failed counts every non-2xx as a failure, which is
// the wrong lens for POST /fund. The service answers a refusal it expects --
// the wallet has no suitable UTXO, the balance is short -- with 409 and a
// machine-readable `code`. That is the wallet running out, not the service
// breaking, and a breakpoint test has to tell the two apart or it will report
// the wallet's limit as the service's.
//
// See ErrorCode::status in src/responses.rs for the full mapping.

import { Counter, Rate, Trend } from 'k6/metrics';

/** 200: funded, outpoints returned. */
export const fundOk = new Rate('fund_ok');

/** 5xx: the service or its upstream broke. This is the breakpoint signal. */
export const fundFailed = new Rate('fund_failed');

/** 409: well-formed but refused against current state -- usually the wallet. */
export const fundRefused = new Rate('fund_refused');

/** 429: the configured [web_interface.rate_limit] turned the request away. */
export const fundThrottled = new Rate('fund_throttled');

/** 422: some transactions broadcast and some did not. Needs a human. */
export const fundPartial = new Counter('fund_partial');

/** 200 with `replayed: true` -- an idempotency replay, not fresh funding. */
export const fundReplayed = new Counter('fund_replayed');

/**
 * 400/401/404: the test itself is wrong -- bad body, missing key, unknown
 * client. Any of these means the run is measuring nothing.
 */
export const fundMisconfigured = new Counter('fund_misconfigured');

/** Latency of the /fund call. Separate from http_req_duration so a mixed run keeps it clean. */
export const fundDuration = new Trend('fund_duration', true);

/** Every outcome, tagged by the service's `code`, e.g. fund_outcome{code:no_suitable_utxo}. */
export const fundOutcome = new Counter('fund_outcome');

/**
 * Every `code` POST /fund can answer with, as ErrorCode serialises it --
 * snake_case, pinned by `error_codes_serialise_to_their_documented_strings`
 * in src/responses.rs -- plus `ok` for a 200.
 */
export const FUND_CODES = [
  'ok',
  'insufficient_balance',
  'no_suitable_utxo',
  'broadcast_failed',
  'broadcast_rejected',
  'broadcast_outcome_unknown',
  'partial_broadcast',
  'chain_unavailable',
  'rate_limited',
  'key_in_progress',
  'idempotency_key_reused',
  'internal',
  'invalid_request',
  'unauthorized',
  'unknown_client',
];

/**
 * Thresholds that make the per-code breakdown of `fund_outcome` appear in the
 * summary.
 *
 * k6 prints a sub-metric only when a threshold is declared for it, and a
 * threshold has to be declared in the init context, before any code has been
 * seen. So every code gets a `count>=0` -- always true, never a failure -- and
 * is there purely to make k6 render the row. Without this the summary shows
 * one `fund_outcome` total and no way to tell which refusal it was.
 *
 * Codes that never occurred show as 0, which is itself worth seeing: it is the
 * difference between "no request was rate limited" and "rate limiting was
 * never in play".
 */
export function outcomeThresholds() {
  const thresholds = {};
  for (const code of FUND_CODES) {
    thresholds[`fund_outcome{code:${code}}`] = ['count>=0'];
  }
  return thresholds;
}
