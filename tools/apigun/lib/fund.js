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
    if (config.logOutpoints) {
      printOutpoints(res);
    }
  } else if (outcome === 'partial') {
    fundPartial.add(1);
    // A partial broadcast still hands the caller real outpoints for the
    // transactions that did go out, so they belong in the same check.
    if (config.logOutpoints) {
      printOutpoints(res);
    }
  } else if (outcome === 'misconfigured') {
    fundMisconfigured.add(1);
    console.error(`/fund ${res.status} ${code}: ${res.body}`);
  }

  return { res, outcome, code };
}

/**
 * Print every outpoint this response handed back, one per line as
 * `outpoint=<hash>:<index>`.
 *
 * The same outpoint appearing twice across the output means two calls were
 * each told they own the same output, and only one of them can spend it.
 * Concurrent requests can select the same UTXO and, since signing is
 * deterministic, build the identical transaction; the second broadcast then
 * comes back as "already known", which counts as success. Both callers get a
 * 200 and nothing is logged as an error, so the responses are the only place
 * this is visible.
 *
 * Replays are skipped: an idempotency replay returning the outpoints of the
 * original call is what `replayed` exists to signal, not a duplicate.
 */
function printOutpoints(res) {
  try {
    if (res.json('replayed') === true) {
      return;
    }
    const outpoints = res.json('outpoints') || [];
    for (let i = 0; i < outpoints.length; i++) {
      const outpoint = outpoints[i];
      if (outpoint && outpoint.hash !== undefined && outpoint.index !== undefined) {
        console.log(`outpoint=${outpoint.hash}:${outpoint.index}`);
      }
    }
  } catch (_) {
    // A body that will not parse is already counted as a failed check.
  }
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
