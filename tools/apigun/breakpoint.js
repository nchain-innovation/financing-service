// Breakpoint test: at what request rate does POST /fund stop working?
//
//   k6 run tools/apigun/breakpoint.js
//   k6 run -e RATES=1,2,5,10,20,40 -e STAGE_DURATION=30s tools/apigun/breakpoint.js
//
// Holds each rate in RATES for STAGE_DURATION and reports per-rate success and
// latency, so the summary reads as a table: the highest rate whose
// fund_ok{rate:N} threshold still passes is the answer.
//
// Arrival-rate, not VUs, on purpose. A constant-VUs run slows itself down as
// the service slows, so it can never overload anything -- it measures latency
// at a fixed concurrency, not a breaking point. An arrival-rate executor keeps
// sending at the target rate regardless, which is what a real caller does.
//
// This spends real UTXOs at every rate it reaches. Run it against regtest or a
// wallet you are willing to drain, and read the wallet caveat in README.md
// before trusting the number it gives you.

import exec from 'k6/execution';
import { config } from './lib/config.js';
import { fund } from './lib/fund.js';
import { outcomeThresholds } from './lib/metrics.js';

// --- Load profile -----------------------------------------------------------

const RATES = (__ENV.RATES || '1,2,5,10,20')
  .split(',')
  .map((r) => Number(r.trim()))
  .filter((r) => Number.isFinite(r) && r > 0);

const STAGE_DURATION = __ENV.STAGE_DURATION || '30s';
const RAMP_DURATION = __ENV.RAMP_DURATION || '5s';
const MAX_RATE = Math.max(...RATES);

// k6 sorts sub-metrics lexicographically and offers no hook to change that, so
// a bare rate tag comes out as 1, 10, 2, 20, 5. Zero-padding to the width of
// the largest rate makes the string order match the numeric order, which is
// what makes the summary readable top to bottom.
const RATE_LABEL_WIDTH = String(Math.floor(MAX_RATE)).length;

function rateLabel(rate) {
  const text = String(rate);
  const dot = text.indexOf('.');
  const whole = dot === -1 ? text : text.slice(0, dot);
  const fraction = dot === -1 ? '' : text.slice(dot);
  // padStart is ES2017 and this has to run on whatever engine k6 ships, so pad
  // by hand.
  let padding = '';
  for (let i = whole.length; i < RATE_LABEL_WIDTH; i++) {
    padding += '0';
  }
  return padding + whole + fraction;
}

// Latency the service is expected to hold. Funding waits on a broadcast, so
// this is upstream-dominated; raise it when pointing at mainnet.
const LATENCY_BUDGET = Number(__ENV.LATENCY_BUDGET || 3000);

// The success rate a stage must hold to count as sustained. 0.95 tolerates the
// odd blip while still marking the knee; set 1.0 if "max TPS" means every
// request succeeded, which is the stricter and more common definition.
const SUCCESS_THRESHOLD = __ENV.SUCCESS_THRESHOLD || '0.95';

// Each rate gets a short ramp and then a plateau, so the numbers tagged with
// that rate come from a period when it was actually being sustained.
const stages = [];
for (const rate of RATES) {
  stages.push({ target: rate, duration: RAMP_DURATION });
  stages.push({ target: rate, duration: STAGE_DURATION });
}

// An iteration blocks on a broadcast, so the VUs needed are roughly
// rate x seconds-per-request. These defaults assume about a second; raise
// MAX_VUS if k6 warns that it cannot keep up with the arrival rate, because
// that warning means the reported rate was never actually reached.
const PRE_ALLOCATED_VUS = Number(__ENV.PRE_ALLOCATED_VUS || Math.max(10, MAX_RATE * 2));
const MAX_VUS = Number(__ENV.MAX_VUS || Math.max(50, MAX_RATE * 10));

// --- Thresholds -------------------------------------------------------------
//
// One pair per rate, which is the whole point of the script: the summary then
// lists every rate with a pass or a fail beside it. These do not abort -- a
// breakpoint test is meant to run past the breaking point so you can see how
// far past it degrades.

const thresholds = {
  // A bad body, a missing API key or an unknown client means the run measured
  // nothing at all. Stop immediately rather than produce a confident graph of
  // 400s.
  fund_misconfigured: [{ threshold: 'count==0', abortOnFail: true }],
  // Not assertions -- these exist so the per-code breakdown is printed at all.
  ...outcomeThresholds(),
};

for (const rate of RATES) {
  thresholds[`fund_ok{rate:${rateLabel(rate)}}`] = [
    SUCCESS_THRESHOLD === '1' || SUCCESS_THRESHOLD === '1.0'
      ? 'rate==1.0'
      : `rate>${SUCCESS_THRESHOLD}`,
  ];
  thresholds[`fund_duration{rate:${rateLabel(rate)}}`] = [`p(95)<${LATENCY_BUDGET}`];
}

export const options = {
  scenarios: {
    breakpoint: {
      executor: 'ramping-arrival-rate',
      startRate: RATES[0],
      timeUnit: '1s',
      preAllocatedVUs: PRE_ALLOCATED_VUS,
      maxVUs: MAX_VUS,
      stages,
    },
  },
  thresholds,
};

// --- Which rate are we at? --------------------------------------------------
//
// k6 does not expose the arrival rate a scenario is currently targeting, so
// derive it from elapsed time against the same stage list that built the
// profile. During a ramp the rate is in motion and the sample would be
// misleading, so those requests are tagged `ramp` and kept out of the per-rate
// thresholds above.

const timeline = [];
{
  let elapsed = 0;
  for (const rate of RATES) {
    elapsed += durationMs(RAMP_DURATION);
    timeline.push({ until: elapsed, label: 'ramp' });
    elapsed += durationMs(STAGE_DURATION);
    timeline.push({ until: elapsed, label: rateLabel(rate) });
  }
}

function durationMs(spec) {
  const match = /^(\d+(?:\.\d+)?)(ms|s|m|h)$/.exec(String(spec).trim());
  if (!match) throw new Error(`unrecognised duration "${spec}"`);
  const value = Number(match[1]);
  const unit = { ms: 1, s: 1000, m: 60000, h: 3600000 }[match[2]];
  return value * unit;
}

function currentRateLabel() {
  const elapsed = exec.instance.currentTestRunDuration;
  for (const slot of timeline) {
    if (elapsed < slot.until) return slot.label;
  }
  // Past the last stage, which happens while the executor drains.
  return 'ramp';
}

// --- Test -------------------------------------------------------------------

/**
 * Set a VU-level tag, which applies to every metric the VU emits for the rest
 * of the iteration -- including the custom metrics in lib/fund.js, which a tag
 * on the request options would miss.
 *
 * k6 moved this from `exec.vu.tags` to `exec.vu.metrics.tags` in v0.47, so
 * take whichever the installed version has.
 */
function tagVu(key, value) {
  const bag = (exec.vu.metrics && exec.vu.metrics.tags) || exec.vu.tags;
  if (!bag) throw new Error('this k6 version exposes no VU tag API');
  bag[key] = value;
}

export default function () {
  tagVu('rate', currentRateLabel());
  fund();
}

export function teardown() {
  console.log(
    `breakpoint: ${config.clientId} @ ${config.baseUrl}, rates ${RATES.join(', ')} req/s, ` +
      `${STAGE_DURATION} each. The highest rate whose fund_ok{rate:N} threshold passed ` +
      `is the rate the service sustained. Rate tags are zero-padded so they sort ` +
      `in numeric order: ${RATES.map(rateLabel).join(', ')}.`,
  );
}
