// Shared configuration for the apigun scripts.
//
// Everything is read from environment variables in the init context, so a
// script can be pointed at a local service, a regtest box or a deployed
// instance without editing it. Defaults match data/financing-service.toml.

function num(name, fallback) {
  const raw = __ENV[name];
  if (raw === undefined || raw === '') return fallback;
  const parsed = Number(raw);
  if (!Number.isFinite(parsed)) {
    throw new Error(`${name} must be a number, got "${raw}"`);
  }
  return parsed;
}

function bool(name, fallback) {
  const raw = __ENV[name];
  if (raw === undefined || raw === '') return fallback;
  return raw === 'true' || raw === '1';
}

function required(name) {
  const raw = __ENV[name];
  if (raw === undefined || raw === '') {
    throw new Error(`${name} is required: pass it with -e ${name}=<value>`);
  }
  return raw.trim();
}

export const config = {
  baseUrl: (__ENV.BASE_URL || 'http://127.0.0.1:9080').replace(/\/$/, ''),
  clientId: __ENV.CLIENT_ID || 'event-rs',
  // Sent as X-API-Key. Empty when the client is configured without a key.
  apiKey: __ENV.API_KEY || '',

  satoshi: num('SATOSHI', 100),
  noOfOutpoints: num('NO_OF_OUTPOINTS', 1),
  // One transaction per outpoint (true) or one carrying all of them (false).
  multipleTx: bool('MULTIPLE_TX', false),
  // Hex locking script the funded outputs pay to. Required, with no default:
  // every request spends real coins and sends them to whatever this names, so
  // the caller has to say where. See docs/LockingScripts.md.
  lockingScript: required('LOCKING_SCRIPT'),

  // 'off' sends no idempotency_key; 'unique' sends a fresh one per iteration;
  // 'replay' reuses one key for the whole run, exercising the replay path
  // instead of funding.
  idempotency: __ENV.IDEMPOTENCY || 'off',
  replayKey: `apigun-replay-${__ENV.RUN_ID || Date.now()}`,

  // Print every outpoint returned, for the duplicate check in the README.
  // Off by default: it is a console write per outpoint, which costs enough to
  // distort a run you take a TPS number from.
  logOutpoints: bool('LOG_OUTPOINTS', false),

  // Generous by default: funding waits on a broadcast, and a timeout here
  // would be recorded as a service failure when it is really the test giving
  // up early.
  timeout: __ENV.TIMEOUT || '30s',
};

export function headers() {
  const h = { 'Content-Type': 'application/json' };
  if (config.apiKey) {
    h['X-API-Key'] = config.apiKey;
  }
  return h;
}
