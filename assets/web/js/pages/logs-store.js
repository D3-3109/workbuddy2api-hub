// Log buffer for the Logs page: ingestion, per-level / per-tag counters and filtering.
// DOM free on purpose (unit-testable with node). The server ring holds 2000 lines and
// ids are contiguous and monotonic (also across /logs/clear), but restart at 1 after a
// gateway restart, which is what staleReason() detects.

export const BUFFER_MAX = 2000;
export const LEVELS = ['INFO', 'WARN', 'ERROR', 'DEBUG'];

/** Normalise one server row into the shape the page works with. */
export function prep(r) {
  const level = String(r.level || 'INFO').toUpperCase();
  const tag = String(r.tag || '');
  const msg = String(r.msg == null ? '' : r.msg);
  const time = String(r.time || String(r.ts || '').slice(11, 19));
  return {
    id: Number(r.id) || 0,
    at: Number(r.at) || 0,
    ts: String(r.ts || ''),
    time,
    level: level === 'WARNING' ? 'WARN' : level,
    tag,
    tk: tag.toLowerCase(),
    msg,
    // what the search box looks at (spec: msg + tag + time)
    lc: (msg + ' ' + tag + ' ' + time).toLowerCase(),
  };
}

/** Predicate for the current filters. `query` must already be trimmed + lower-cased. */
export function makeMatcher({ level = 'ALL', tag = '', query = '' } = {}) {
  const anyLevel = level === 'ALL';
  return (e) => (anyLevel || e.level === level) && (!tag || e.tk === tag) && (!query || e.lc.includes(query));
}

export const formatLine = (e) => `[${e.ts || e.time}] [${e.level}] [${e.tag}] ${e.msg}`;

export function createStore(max = BUFFER_MAX) {
  let entries = [];
  const counts = { INFO: 0, WARN: 0, ERROR: 0, DEBUG: 0 };
  const tags = new Map(); // lower-case tag -> { tag, n }
  const st = { lastId: 0, lastAt: 0, total: 0, stderrDropped: 0, stderrClosed: false };

  function tally(e, d) {
    if (Object.hasOwn(counts, e.level)) counts[e.level] += d;
    const cur = tags.get(e.tk);
    if (cur) { cur.n += d; if (cur.n <= 0) tags.delete(e.tk); }
    else if (d > 0) tags.set(e.tk, { tag: e.tag, n: d });
  }

  function meta(reply) {
    st.total = reply.total == null ? -1 : Number(reply.total) || 0; // -1: server did not say
    st.stderrDropped = Number(reply.stderr_dropped) || 0;
    st.stderrClosed = !!reply.stderr_closed;
  }

  const store = {
    get entries() { return entries; },
    counts, tags,
    get lastId() { return st.lastId; },
    get total() { return st.total < 0 ? entries.length : st.total; },
    get stderrDropped() { return st.stderrDropped; },
    get stderrClosed() { return st.stderrClosed; },

    /**
     * Why the incremental reply cannot be merged ('' when it can):
     *  restart  the gateway restarted (ids start at 1 again, or the first new line is older than ours)
     *  gap      lines between our last id and the reply were dropped (long pause, or cleared elsewhere)
     */
    staleReason(reply) {
      if (!st.lastId) return '';
      const maxId = Number(reply.max_id);
      if (Number.isFinite(maxId) && maxId < st.lastId) return 'restart';
      const first = reply.logs && reply.logs[0];
      if (!first) return '';
      if (first.id > st.lastId + 1) return 'gap';
      if (st.lastAt && first.at < st.lastAt - 1) return 'restart';
      return '';
    },

    /** After append(): true when we still hold lines the server no longer has (cleared elsewhere). */
    isStale() { return st.total >= 0 && entries.length > st.total; },

    /** Replace the buffer with a full (since_id=0) reply. */
    reset(reply) {
      const before = entries.length + ':' + st.lastId;
      const rows = (reply.logs || []).map(prep);
      entries = rows.length > max ? rows.slice(-max) : rows;
      counts.INFO = counts.WARN = counts.ERROR = counts.DEBUG = 0;
      tags.clear();
      for (const e of entries) tally(e, 1);
      const last = entries[entries.length - 1];
      st.lastId = Math.max(Number(reply.max_id) || 0, last ? last.id : 0);
      st.lastAt = last ? last.at : 0;
      meta(reply);
      return { reset: true, changed: before !== entries.length + ':' + st.lastId, fresh: [], dropped: [] };
    },

    /** Merge an incremental (since_id=lastId) reply. Dedupes by id. */
    append(reply) {
      const base = st.lastId;
      const fresh = [];
      for (const r of reply.logs || []) {
        if (!(r.id > base)) continue;
        const e = prep(r);
        entries.push(e);
        tally(e, 1);
        fresh.push(e);
        if (e.id > st.lastId) { st.lastId = e.id; st.lastAt = e.at; }
      }
      const maxId = Number(reply.max_id);
      if (Number.isFinite(maxId) && maxId > st.lastId) st.lastId = maxId;
      let dropped = [];
      if (entries.length > max) {
        dropped = entries.splice(0, entries.length - max);
        for (const d of dropped) tally(d, -1);
      }
      meta(reply);
      return { reset: false, changed: fresh.length > 0, fresh, dropped };
    },

    clear() {
      entries = [];
      counts.INFO = counts.WARN = counts.ERROR = counts.DEBUG = 0;
      tags.clear();
      st.lastId = 0; st.lastAt = 0; st.total = 0;
    },
  };
  return store;
}
