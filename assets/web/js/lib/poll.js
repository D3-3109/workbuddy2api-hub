// Visibility-aware polling. Replaces five always-on setInterval loops.
//  - one timer chain per poller, never overlapping (next tick is scheduled after
//    the previous one settles)
//  - nothing runs while the tab is hidden; returning to the tab refreshes only
//    pollers whose data is older than their interval
//  - errors back off exponentially up to 60 s instead of hammering the server
//  - stop() aborts the in-flight request

const active = new Set();

function onVisibility() {
  for (const p of active) {
    if (document.hidden) p._clear();
    else p._resume();
  }
}
document.addEventListener('visibilitychange', onVisibility);

export function poller(fn, interval, { immediate = true } = {}) {
  let timer = 0;
  let running = false;
  let ctl = null;
  let fails = 0;
  let last = 0;
  let started = false;

  const p = {
    _clear() { clearTimeout(timer); timer = 0; },
    _schedule(delay) {
      p._clear();
      if (!started || document.hidden) return;
      timer = setTimeout(tick, delay);
    },
    _resume() {
      if (!started || running) return;
      const age = Date.now() - last;
      p._schedule(Math.max(0, interval - age));
    },
    start() {
      if (started) return p;
      started = true;
      active.add(p);
      if (immediate && !document.hidden) tick();
      else p._schedule(immediate ? 0 : interval);
      return p;
    },
    stop() {
      started = false;
      active.delete(p);
      p._clear();
      if (ctl) ctl.abort();
    },
    /** Run now (e.g. after a user action) unless a run is already in flight. */
    trigger() { if (started && !running) { p._clear(); tick(); } },
    get running() { return running; },
    setInterval(ms) { interval = ms; if (started && !running) p._schedule(ms); },
  };

  async function tick() {
    if (!started || running) return;
    running = true;
    ctl = new AbortController();
    try {
      await fn(ctl.signal);
      fails = 0;
    } catch (e) {
      if (!(e && e.name === 'AbortError')) fails++;
    } finally {
      running = false;
      last = Date.now();
      ctl = null;
      if (started) p._schedule(fails ? Math.min(interval * 2 ** fails, 60000) : interval);
    }
  }

  return p;
}
