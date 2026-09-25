// The POST /fund request itself: body, call, classification, metrics.
//
// Both scripts share this so that smoke and breakpoint runs are measuring the
// same thing and their numbers are comparable.

import http from 'k6/http';
import { config, headers } from './config.js';
import {
  fundDuration,
  fundFailed,
  fundMisconfigured,
  fundOk,
  fundOutcome,
  fundPartial,
  fundRefused,
  fundReplayed,
  fundThrottled,
} from './metrics.js';

export function buildBody() {
  const body = {
    client_id: config.clientId,
    satoshi: config.satoshi,
    no_of_outpoints: config.noOfOutpoints,
    multiple_tx: config.multipleTx,
    locking_script: config.lockingScript,
  };

  // `locking_scripts` is the alternative form -- one script per outpoint, each
  // independently spendable, and mutually exclusive with `locking_script`. To
  // use it, drop `locking_script` and `no_of_outpoints` and set:
  //
  //   body.locking_scripts = Array(config.noOfOutpoints).fill(config.lockingScript);

  if (config.idempotency === 'unique') {
    body.idempotency_key = `apigun-${__VU}-${__ITER}-${Date.now()}`;
  } else if (config.idempotency === 'replay') {
    body.idempotency_key = config.replayKey;
  }

  return body;
}

/**
 * Classify a response the way the service intends it to be classified.
 *
 * Returns one of: ok, refused, throttled, partial, failed, misconfigured,
 * unexpected.
 */
export function classify(res) {
  if (res.status === 200) return 'ok';
  if (res.status === 409) return 'refused';
  // A 503 that is the wallet, not the service: every UTXO is claimed by
  // requests still broadcasting (CS-475). It is a 503 so that retry policies
  // retry it, but for a breakpoint test it is the wallet's shape limiting the
  // rate -- what no_suitable_utxo meant under load before -- and counting it
  // as a failure would report the wallet's limit as the service's.
  if (res.status === 503 && errorCode(res) === 'funds_in_flight') return 'refused';
  if (res.status === 429) return 'throttled';
  if (res.status === 422) return 'partial';
  // 0 is k6's own "no response" -- a timeout or a refused connection. It is a
  // failure of the service to answer, so it belongs with the 5xx.
  if (res.status === 0 || res.status >= 500) return 'failed';
  if (res.status === 400 || res.status === 401 || res.status === 404) {
    return 'misconfigured';
  }
  return 'unexpected';
}

/** The service's `code` field, or a status-derived label when the body is not JSON. */
export function errorCode(res) {
  try {
    return res.json('code') || `http_${res.status}`;
  } catch (_) {
    return `http_${res.status}`;
  }
}

/**
 * Send one funding request and record it. Returns the outcome of `classify`,
 * so a caller can decide what to do about it.
 */
export function fund() {
  const res = http.post(`${config.baseUrl}/fund`, JSON.stringify(buildBody()), {
    headers: headers(),
    timeout: config.timeout,
    tags: { endpoint: 'fund' },
  });

  const outcome = classify(res);
  const code = outcome === 'ok' ? 'ok' : errorCode(res);

  fundDuration.add(res.timings.duration);
  fundOutcome.add(1, { code, outcome });

  // Every Rate gets a sample on every request, so the rates are fractions of
  // all requests rather than of the ones that happened to reach that branch.
  fundOk.add(outcome === 'ok');
  fundFailed.add(outcome === 'failed');
  fundRefused.add(outcome === 'refused');
  fundThrottled.add(outcome === 'throttled');

  if (outcome === 'ok') {
    // Absent means false. Present and true means these outpoints are a replay
    // of an earlier funding, not fresh ones.
    if (res.json('replayed') === true) {
      fundReplayed.add(1);
    }
  } else if (outcome === 'partial') {
    fundPartial.add(1);
  } else if (outcome === 'misconfigured') {
    fundMisconfigured.add(1);
    console.error(`/fund ${res.status} ${code}: ${res.body}`);
  }

  return { res, outcome, code };
}

/** True when the body really carries the outpoints that were asked for. */
export function hasExpectedOutpoints(res) {
  if (res.status !== 200) return false;
  try {
    const outpoints = res.json('outpoints');
    return Array.isArray(outpoints) && outpoints.length === config.noOfOutpoints;
  } catch (_) {
    return false;
  }
}
