// Smoke test: does POST /fund work at all, one request at a time.
//
//   k6 run tools/apigun/smoke.js
//
// No load, no concurrency. A handful of sequential fundings with strict
// thresholds, so a failure means the endpoint or the wallet is broken rather
// than overloaded. Run this before breakpoint.js -- a breakpoint run against a
// service that never worked measures nothing.
//
// This spends real UTXOs: every passing iteration broadcasts a transaction.

import { check, sleep } from 'k6';
import { config } from './lib/config.js';
import { fund, hasExpectedOutpoints } from './lib/fund.js';

export const options = {
  scenarios: {
    smoke: {
      executor: 'shared-iterations',
      vus: 1,
      iterations: Number(__ENV.ITERATIONS || 5),
      maxDuration: '2m',
    },
  },
  thresholds: {
    // Nothing is allowed to fail at this level.
    fund_ok: ['rate==1.0'],
    fund_failed: ['rate==0'],
    fund_refused: ['rate==0'],
    fund_misconfigured: ['count==0'],
    checks: ['rate==1.0'],
    // Loose on purpose. A smoke run is about correctness; breakpoint.js is
    // where latency is the subject.
    fund_duration: ['p(95)<10000'],
  },
};

export default function () {
  const { res, outcome, code } = fund();

  check(res, {
    'status is 200': () => outcome === 'ok',
    'outpoints match the request': () => hasExpectedOutpoints(res),
  });

  if (outcome !== 'ok') {
    console.error(`iteration failed: ${outcome} (${code}) ${res.status} ${res.body}`);
  }

  sleep(Number(__ENV.SLEEP || 1));
}

export function teardown() {
  console.log(
    `smoke: ${config.clientId} @ ${config.baseUrl}, ` +
      `${config.satoshi} sat x ${config.noOfOutpoints} outpoint(s) per request`,
  );
}
