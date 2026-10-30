// Small SVG chart kit. Charts redraw only on data/size change, hover is one
// rAF-throttled pointer listener, and node counts stay O(points x series).
import { h, s, rafThrottle } from './dom.js';

const SERIES_COLORS = ['var(--c1)', 'var(--c2)', 'var(--c3)', 'var(--c4)', 'var(--c5)', 'var(--c6)', 'var(--c7)', 'var(--c8)'];
export const seriesColor = (i) => SERIES_COLORS[i % SERIES_COLORS.length];

/** Round a maximum up to a "nice" axis value and return ticks. */
export function niceScale(max, count = 4) {
  if (!(max > 0)) return { max: 1, ticks: [0, 1] };
  const raw = max / count;
  const mag = 10 ** Math.floor(Math.log10(raw));
  const norm = raw / mag;
  const step = (norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 2.5 ? 2.5 : norm <= 5 ? 5 : 10) * mag;
  const top = Math.ceil(max / step) * step;
  const ticks = [];
  for (let v = 0; v <= top + step / 1e6; v += step) ticks.push(+v.toFixed(10));
  return { max: top, ticks };
}

/** Monotone cubic interpolation (no overshoot) as an SVG path through [x,y] points. */
function smoothPath(pts) {
  const n = pts.length;
  if (n === 0) return '';
  if (n < 3) return 'M' + pts.map((p) => p[0].toFixed(1) + ',' + p[1].toFixed(1)).join('L');
  const dx = [], dy = [], m = [];
  for (let i = 0; i < n - 1; i++) { dx[i] = pts[i + 1][0] - pts[i][0]; dy[i] = pts[i + 1][1] - pts[i][1]; m[i] = dx[i] ? dy[i] / dx[i] : 0; }
  const tan = [m[0]];
  for (let i = 1; i < n - 1; i++) tan[i] = m[i - 1] * m[i] <= 0 ? 0 : (m[i - 1] + m[i]) / 2;
  tan[n - 1] = m[n - 2];
  for (let i = 0; i < n - 1; i++) {
    if (m[i] === 0) { tan[i] = 0; tan[i + 1] = 0; continue; }
    const a = tan[i] / m[i], b = tan[i + 1] / m[i], r = a * a + b * b;
    if (r > 9) { const k = 3 / Math.sqrt(r); tan[i] = k * a * m[i]; tan[i + 1] = k * b * m[i]; }
  }
  let d = `M${pts[0][0].toFixed(1)},${pts[0][1].toFixed(1)}`;
  for (let i = 0; i < n - 1; i++) {
    const x0 = pts[i][0], y0 = pts[i][1], x1 = pts[i + 1][0], y1 = pts[i + 1][1], w = dx[i] / 3;
    d += `C${(x0 + w).toFixed(1)},${(y0 + tan[i] * w).toFixed(1)} ${(x1 - w).toFixed(1)},${(y1 - tan[i + 1] * w).toFixed(1)} ${x1.toFixed(1)},${y1.toFixed(1)}`;
  }
  return d;
}

let gradSeq = 0;

/**
 * Time-series chart. update({ x: number[], series: [{ name, values: number[], color? }] })
 * opts: { height, type: 'area'|'bar', stacked, fmtX(x,i), fmtTipX(x,i), fmtY(v), fmtTip(v), maxTicksX }
 */
export function timeChart(host, opts = {}) {
  const H = opts.height || 260;
  const L = 52, R = 14, T = 12, B = 26;
  const type = opts.type || 'area';
  const fmtY = opts.fmtY || String;
  const fmtTip = opts.fmtTip || fmtY;
  const fmtX = opts.fmtX || String;
  const fmtTipX = opts.fmtTipX || fmtX;
  const uid = 'g' + ++gradSeq;
  let data = { x: [], series: [] };
  let W = 0;
  let geo = null;

  host.classList.add('chart');
  host.style.height = H + 'px';
  const svg = s('svg', { width: '100%', height: H, role: 'img' });
  const tip = h('div.chart-tip', { hidden: true });
  host.append(svg, tip);

  const ro = new ResizeObserver((entries) => {
    const w = Math.round(entries[0].contentRect.width);
    if (w > 0 && w !== W) { W = w; draw(); }
  });
  ro.observe(host);

  function draw() {
    const { x, series } = data;
    svg.replaceChildren();
    geo = null;
    if (!W) return;
    svg.setAttribute('viewBox', `0 0 ${W} ${H}`);
    const n = x.length;
    const pw = W - L - R, ph = H - T - B;
    if (!n || !series.length) return;

    const stacked = !!opts.stacked && series.length > 1;
    let peak = 0;
    for (let i = 0; i < n; i++) {
      if (stacked) { let sum = 0; for (const sr of series) sum += sr.values[i] || 0; peak = Math.max(peak, sum); }
      else for (const sr of series) peak = Math.max(peak, sr.values[i] || 0);
    }
    const sc = niceScale(peak, 4);
    const yOf = (v) => T + ph - (v / sc.max) * ph;
    const band = pw / n;
    const xOf = type === 'bar' ? (i) => L + band * (i + 0.5) : (i) => L + (n === 1 ? pw / 2 : (i / (n - 1)) * pw);

    const defs = s('defs');
    const grid = s('g', { class: 'grid' });
    for (const tk of sc.ticks) {
      const y = yOf(tk);
      grid.append(
        s('line', { x1: L, x2: W - R, y1: y, y2: y, stroke: 'var(--line)', 'stroke-width': 1, 'stroke-dasharray': tk === 0 ? '' : '3 4' }),
        s('text', { x: L - 8, y: y + 4, 'text-anchor': 'end', fill: 'var(--fg-3)', 'font-size': 11 }, fmtY(tk)),
      );
    }
    const want = Math.max(2, Math.min(opts.maxTicksX || 7, Math.floor(pw / 90)));
    const every = Math.max(1, Math.ceil(n / want));
    for (let i = 0; i < n; i += every) {
      grid.append(s('text', { x: xOf(i), y: H - 7, 'text-anchor': i === 0 && type !== 'bar' ? 'start' : 'middle', fill: 'var(--fg-3)', 'font-size': 11 }, fmtX(x[i], i)));
    }
    svg.append(defs, grid);

    const base = new Array(n).fill(0);
    const tops = [];
    series.forEach((sr, si) => {
      const color = sr.color || seriesColor(si);
      const lo = stacked ? base.slice() : null;
      const hi = new Array(n);
      for (let i = 0; i < n; i++) hi[i] = (stacked ? base[i] : 0) + (sr.values[i] || 0);
      if (stacked) for (let i = 0; i < n; i++) base[i] = hi[i];
      tops.push(hi);
      if (type === 'bar') {
        const bw = Math.max(2, Math.min(band * 0.68, 38));
        const g = s('g', { fill: color });
        for (let i = 0; i < n; i++) {
          const v = sr.values[i] || 0;
          if (!v) continue;
          const y1 = yOf(hi[i]), y0 = yOf(stacked ? lo[i] : 0);
          g.append(s('rect', { x: xOf(i) - bw / 2, y: y1, width: bw, height: Math.max(1, y0 - y1), rx: Math.min(3, bw / 2) }));
        }
        svg.append(g);
        return;
      }
      const pts = hi.map((v, i) => [xOf(i), yOf(v)]);
      const line = smoothPath(pts);
      const gid = `${uid}-${si}`;
      defs.append(s('linearGradient', { id: gid, x1: 0, y1: 0, x2: 0, y2: 1 },
        s('stop', { offset: '0%', 'stop-color': color, 'stop-opacity': stacked ? 0.55 : 0.28 }),
        s('stop', { offset: '100%', 'stop-color': color, 'stop-opacity': stacked ? 0.12 : 0.02 })));
      const floor = stacked
        ? 'L' + lo.map((v, i) => [xOf(i), yOf(v)]).reverse().map((p) => p[0].toFixed(1) + ',' + p[1].toFixed(1)).join('L')
        : `L${xOf(n - 1).toFixed(1)},${yOf(0)}L${xOf(0).toFixed(1)},${yOf(0)}`;
      svg.append(
        s('path', { d: line + floor + 'Z', fill: `url(#${gid})` }),
        s('path', { d: line, fill: 'none', stroke: color, 'stroke-width': 2, 'stroke-linejoin': 'round', 'stroke-linecap': 'round' }),
      );
    });

    const cross = s('line', { y1: T, y2: T + ph, stroke: 'var(--line-2)', 'stroke-width': 1, visibility: 'hidden' });
    const dots = series.map((sr, si) => s('circle', { r: 4, fill: 'var(--surface)', stroke: sr.color || seriesColor(si), 'stroke-width': 2, visibility: 'hidden' }));
    const hit = s('rect', { x: L, y: T, width: pw, height: ph, fill: 'transparent', style: { cursor: 'crosshair' } });
    svg.append(cross, ...(type === 'bar' ? [] : dots), hit);
    geo = { n, pw, ph, band, xOf, yOf, tops, cross, dots, stacked };
    hit.addEventListener('pointermove', onMove);
    hit.addEventListener('pointerleave', hide);
  }

  const onMove = rafThrottle((e) => {
    if (!geo) return;
    const box = svg.getBoundingClientRect();
    const px = e.clientX - box.left;
    const { n, pw, band, xOf, yOf, tops, cross, dots } = geo;
    let i = type === 'bar' ? Math.floor((px - L) / band) : Math.round(((px - L) / pw) * (n - 1));
    i = Math.max(0, Math.min(n - 1, i));
    const cx = xOf(i);
    cross.setAttribute('x1', cx); cross.setAttribute('x2', cx);
    cross.setAttribute('visibility', 'visible');
    dots.forEach((d, si) => { d.setAttribute('cx', cx); d.setAttribute('cy', yOf(tops[si][i])); d.setAttribute('visibility', 'visible'); });
    const rows = data.series.map((sr, si) => h('div', { style: 'display:flex;gap:8px;align-items:center;justify-content:space-between' },
      h('span', h('i', { style: `display:inline-block;width:8px;height:8px;border-radius:2px;margin-right:6px;background:${sr.color || seriesColor(si)}` }), sr.name),
      h('b', { style: 'font-variant-numeric:tabular-nums' }, fmtTip(sr.values[i] || 0))));
    tip.replaceChildren(h('div', { style: 'color:var(--fg-3);margin-bottom:4px' }, fmtTipX(data.x[i], i)), ...rows);
    tip.hidden = false;
    const tw = tip.offsetWidth;
    tip.style.left = Math.max(tw / 2, Math.min(cx, W - tw / 2)) + 'px';
    tip.style.top = Math.max(tip.offsetHeight + 6, yOf(Math.max(...tops.map((t) => t[i])))) - 8 + 'px';
  });
  function hide() {
    tip.hidden = true;
    if (!geo) return;
    geo.cross.setAttribute('visibility', 'hidden');
    geo.dots.forEach((d) => d.setAttribute('visibility', 'hidden'));
  }

  return {
    update(next) { data = next; hide(); draw(); },
    dispose() { ro.disconnect(); },
  };
}

/** Tiny inline trend line: returns an <svg>. */
export function sparkline(values, { color = 'var(--accent)', fill = true } = {}) {
  const n = values.length;
  const svg = s('svg', { viewBox: '0 0 100 36', preserveAspectRatio: 'none', width: '100%', height: '100%' });
  if (n < 2) return svg;
  const max = Math.max(...values, 1e-9);
  const pts = values.map((v, i) => [(i / (n - 1)) * 100, 33 - (v / max) * 29]);
  const line = smoothPath(pts);
  if (fill) svg.append(s('path', { d: `${line}L100,36L0,36Z`, fill: color, opacity: 0.14 }));
  svg.append(s('path', { d: line, fill: 'none', stroke: color, 'stroke-width': 1.6, 'vector-effect': 'non-scaling-stroke', 'stroke-linejoin': 'round' }));
  return svg;
}

/** Donut: parts = [{ label, value, color }] */
export function donut(parts, { size = 150, thickness = 16, centerTop = '', centerBottom = '' } = {}) {
  const r = (size - thickness) / 2, c = size / 2, circ = 2 * Math.PI * r;
  const total = parts.reduce((a, p) => a + (p.value || 0), 0);
  const svg = s('svg', { width: size, height: size, viewBox: `0 0 ${size} ${size}`, role: 'img' });
  svg.append(s('circle', { cx: c, cy: c, r, fill: 'none', stroke: 'var(--surface-3)', 'stroke-width': thickness }));
  let off = 0;
  if (total > 0) {
    parts.forEach((p, i) => {
      const len = ((p.value || 0) / total) * circ;
      if (len <= 0) return;
      svg.append(s('circle', {
        cx: c, cy: c, r, fill: 'none', stroke: p.color || seriesColor(i), 'stroke-width': thickness,
        'stroke-dasharray': `${Math.max(0, len - (parts.length > 1 ? 2 : 0))} ${circ}`, 'stroke-dashoffset': -off,
        transform: `rotate(-90 ${c} ${c})`, 'stroke-linecap': 'butt',
      }));
      off += len;
    });
  }
  const wrap = h('div', { style: `position:relative;width:${size}px;height:${size}px;flex:none` }, svg,
    h('div', { style: 'position:absolute;inset:0;display:grid;place-content:center;text-align:center;line-height:1.2' },
      h('div', { style: 'font-size:20px;font-weight:700;font-variant-numeric:tabular-nums' }, centerTop),
      h('div', { style: 'font-size:11.5px;color:var(--fg-3)' }, centerBottom)));
  return wrap;
}

/** 7 x 24 activity heat-map (rows = weekdays, cols = hours). */
export function heatmap(host, { rowLabels, tipFn }) {
  host.classList.add('heat');
  host.style.cssText = 'display:grid;grid-template-columns:34px repeat(24,minmax(0,1fr));gap:3px;align-items:center';
  const cells = [];
  host.append(h('span'));
  for (let c = 0; c < 24; c++) host.append(h('span', { style: 'font-size:10.5px;color:var(--fg-3);text-align:center' }, c % 3 === 0 ? String(c) : ''));
  rowLabels.forEach((label, r) => {
    host.append(h('span', { style: 'font-size:11.5px;color:var(--fg-3)' }, label));
    for (let c = 0; c < 24; c++) {
      const el = h('i', { style: 'display:block;aspect-ratio:1;max-height:26px;border-radius:5px;background:var(--hm0)' });
      cells.push(el);
      host.append(el);
    }
  });
  return {
    update(matrix) {
      let max = 0;
      for (const row of matrix) for (const v of row) if (v > max) max = v;
      matrix.forEach((row, r) => row.forEach((v, c) => {
        const el = cells[r * 24 + c];
        const lvl = !v || !max ? 0 : Math.min(4, 1 + Math.floor((v / max) * 3.999));
        const bg = `var(--hm${lvl})`;
        if (el.__bg !== bg) { el.__bg = bg; el.style.background = bg; }
        const tp = v ? tipFn(r, c, v) : '';
        if (el.__tp !== tp) { el.__tp = tp; tp ? el.setAttribute('data-tip', tp) : el.removeAttribute('data-tip'); }
      }));
    },
  };
}
