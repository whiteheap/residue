/*
 * The residue hero: a stack of flat sheets, like linked worktrees, drawn in
 * dim grays one cell (2 CSS px) at a time, after the residue mark. Now and
 * then one idle sheet loosens into dust that drifts off and fades, the sheets
 * above it settle down, and a new sheet gathers on top.
 *
 * The canvas holds one pixel per cell and CSS scales it up with
 * `image-rendering: pixelated`. A frame is a function of the time t alone, so
 * nothing accumulates between frames. It draws 30 frames a second while
 * something moves and 10 while the stack rests, and sleeps on a timer between
 * two frames, so it does not wake on the display frames it skips. It stops
 * while off screen or in a hidden tab, and draws one still frame when the
 * reader prefers reduced motion. Without this script the hero is plain black and reads the
 * same. It also shows the copy button beside the install command. It loads
 * nothing and stores nothing.
 */
(() => {
  "use strict";

  // The copy button beside the install command. Without this script it stays
  // hidden, and one click still selects the whole command (user-select: all).
  for (const button of document.querySelectorAll("button[data-copy]")) {
    const source = document.getElementById(button.dataset.copy);
    const label = button.querySelector(".copy-label");
    if (!source || !label || !navigator.clipboard) continue;
    let reset = 0;
    const show = (text, state) => {
      label.textContent = text;
      button.dataset.state = state;
      clearTimeout(reset);
      reset = setTimeout(() => {
        label.textContent = "Copy";
        button.dataset.state = "";
      }, 2000);
    };
    button.addEventListener("click", () => {
      navigator.clipboard.writeText(source.textContent.trim()).then(
        () => show("Copied", "done"),
        // No clipboard access: select the command for the reader to copy.
        () => {
          getSelection().selectAllChildren(source);
          show("Selected", "");
        },
      );
    });
    button.hidden = false;
  }

  const canvas = document.querySelector("canvas[data-hero]");
  const ctx = canvas && canvas.getContext("2d", { alpha: false });
  if (!ctx) return;

  const CELL = 2;
  const SHEETS = 5;
  // One sheet in the mark's units: the top face A B C D, THICK deep.
  const A = [0, 36];
  const B = [132, 48];
  const C = [168, 12];
  const D = [36, 0];
  const WIDE = 168;
  const THICK = 10;
  // The space between two sheets, in the same units.
  const SPACE = 5;
  // The cycle, in seconds: rest, one sheet loosens, the sheets above settle,
  // a new one gathers on top.
  const REST = 7;
  const LOOSEN = 4;
  const SETTLE_AT = REST + LOOSEN + 0.3;
  const SETTLE = 1.8;
  const ARRIVE_AT = SETTLE_AT + SETTLE + 0.4;
  const ARRIVE = 3;
  const CYCLE = ARRIVE_AT + ARRIVE + 0.5;
  // The first sheet loosens 3.5 s after the page opens.
  const START = REST - 3.5;
  // The brightest gray, out of 255: the page's text is near white, so the
  // stack stays well under it.
  const PEAK = 84;

  const GRAY = new Uint32Array(256);
  for (let g = 0; g < 256; g++) GRAY[g] = 0xff000000 | (g << 16) | (g << 8) | g;

  function hash(x, y, z) {
    let h = Math.imul(x | 0, 0x27d4eb2d) ^ Math.imul(y | 0, 0x165667b1) ^ Math.imul(z | 0, 0x9e3779b1);
    h = Math.imul(h ^ (h >>> 15), 0x85ebca6b);
    h = Math.imul(h ^ (h >>> 13), 0xc2b2ae35);
    return ((h ^ (h >>> 16)) >>> 0) / 4294967296;
  }

  function noise(x, y, z) {
    const xi = Math.floor(x);
    const yi = Math.floor(y);
    const u = (x - xi) * (x - xi) * (3 - 2 * (x - xi));
    const v = (y - yi) * (y - yi) * (3 - 2 * (y - yi));
    const a = hash(xi, yi, z);
    const b = hash(xi + 1, yi, z);
    const c = hash(xi, yi + 1, z);
    const d = hash(xi + 1, yi + 1, z);
    return a + (b - a) * u + (c - a) * v + (a - b - c + d) * u * v;
  }

  const smooth = (k) => {
    const c = Math.min(1, Math.max(0, k));
    return c * c * (3 - 2 * c);
  };

  function inside(poly, x, y) {
    let pos = false;
    let neg = false;
    for (let n = 0; n < poly.length; n++) {
      const [ax, ay] = poly[n];
      const [bx, by] = poly[(n + 1) % poly.length];
      const cross = (bx - ax) * (y - ay) - (by - ay) * (x - ax);
      if (cross > 0) pos = true;
      if (cross < 0) neg = true;
    }
    return !(pos && neg);
  }

  // Ranks values to 0..1 by their spread, so the first cell goes at 0 and
  // the last at 1.
  function spread(values) {
    let lo = Infinity;
    let hi = -Infinity;
    for (const v of values) {
      lo = Math.min(lo, v);
      hi = Math.max(hi, v);
    }
    return values.map((v) => (v - lo) / (hi - lo || 1));
  }

  // One sheet as a list of cells relative to its corner: each cell's gray as
  // a share of PEAK, when it loosens, when it gathers, and whether it drifts
  // off as dust. A cell between the stipples is a near-black body, so a sheet
  // hides what lies behind it.
  function sheet(s, thick, seed) {
    const pt = ([x, y], down = 0) => [x * s, y * s + down];
    const top = [pt(A), pt(B), pt(C), pt(D)];
    const front = [pt(A), pt(B), pt(B, thick), pt(A, thick)];
    const side = [pt(B), pt(C), pt(C, thick), pt(B, thick)];
    const w = Math.ceil(WIDE * s) + 1;
    const h = Math.ceil(B[1] * s) + thick + 1;
    const grid = new Float32Array(w * h).fill(-1);
    const faces = [
      [top, 0.5, 0.1, 0.3],
      [front, 0.4, 0.08, 0.2],
      [side, 0.32, 0.06, 0.16],
    ];
    for (let j = 0; j < h; j++) {
      for (let i = 0; i < w; i++) {
        for (const [poly, stipple, body, density] of faces) {
          if (!inside(poly, i + 0.5, j + 0.5)) continue;
          grid[j * w + i] = hash(i, j, seed) < density ? stipple : body;
          break;
        }
      }
    }
    // The mark's strokes, one cell wide.
    const corner = (p, down = 0) => {
      const [x, y] = pt(p, down);
      return [Math.min(w - 1, Math.round(x)), Math.min(h - 1, Math.round(y))];
    };
    const strokes = [
      [D, 0, C, 0], [C, 0, C, thick], [C, thick, B, thick], [B, thick, A, thick],
      [A, thick, A, 0], [A, 0, D, 0], [A, 0, B, 0], [B, 0, C, 0], [B, 0, B, thick],
    ];
    for (const [p, pd, q, qd] of strokes) {
      let [x, y] = corner(p, pd);
      const [x1, y1] = corner(q, qd);
      const dx = Math.abs(x1 - x);
      const dy = -Math.abs(y1 - y);
      const sx = x < x1 ? 1 : -1;
      const sy = y < y1 ? 1 : -1;
      let err = dx + dy;
      for (;;) {
        grid[y * w + x] = 1;
        if (x === x1 && y === y1) break;
        const e2 = 2 * err;
        if (e2 >= dy) {
          err += dy;
          x += sx;
        }
        if (e2 <= dx) {
          err += dx;
          y += sy;
        }
      }
    }
    const cells = [];
    for (let j = 0; j < h; j++) {
      for (let i = 0; i < w; i++) {
        if (grid[j * w + i] >= 0) cells.push([i, j, grid[j * w + i]]);
      }
    }
    // Loosens in patches from the right, so the dust leaves the way it drifts.
    const out = spread(cells.map(([i, j]) => 0.6 * noise(i / 5, j / 5, seed + 1) + 0.4 * (1 - i / w)));
    // Gathers in patches from the left.
    const gather = spread(cells.map(([i, j]) => 0.6 * noise(i / 4, j / 4, seed + 2) + 0.4 * (i / w)));
    const n = cells.length;
    const S = {
      n,
      x: new Int16Array(n),
      y: new Int16Array(n),
      value: new Float32Array(n),
      out: Float32Array.from(out),
      gather: Float32Array.from(gather),
      dust: [],
    };
    cells.forEach(([i, j, v], k) => {
      S.x[k] = i;
      S.y[k] = j;
      S.value[k] = v;
      // About a third of the lit cells drift off; the rest just go.
      if (v > 0.2 && hash(i, j, seed + 3) < 0.35) {
        S.dust.push({
          k,
          // Cells a second: rightward and a little up, away from the text.
          vx: 3 + 8 * hash(i, j, seed + 4),
          vy: -(0.4 + 2.2 * hash(i, j, seed + 5)),
          life: 1.8 + 2.4 * hash(i, j, seed + 6),
          phase: 6.283 * hash(i, j, seed + 7),
          at: REST + out[k] * LOOSEN,
        });
      }
    });
    return S;
  }

  // The width in CSS px a room [left, top, right, bottom] gives the stack.
  const TALL = B[1] + THICK + (THICK + SPACE) * (SHEETS - 1) + 22 + 24;
  const span = ([left, top, right, bottom]) =>
    Math.min(440, (right - left) * 0.8, ((bottom - top) * 0.85 * WIDE) / TALL);

  // The stack fills the room the text leaves, given in CSS px as
  // [left, top, right, bottom], and centers in it. A room too small for the
  // smallest stack gets none, and the hero stays black, as without this
  // script.
  function layout(w, h, room, seed) {
    const cols = Math.max(1, Math.ceil(w / CELL));
    const rows = Math.max(1, Math.ceil(h / CELL));
    const key = [w, h, ...room.map(Math.round)].join();
    const [left, top, right, bottom] = room;
    const wide = Math.max(100, span(room));
    const s = wide / CELL / WIDE;
    const thick = Math.max(3, Math.round(THICK * s));
    const pitch = thick + Math.max(2, Math.round(SPACE * s));
    const stackH = Math.round(B[1] * s) + thick + pitch * (SHEETS - 1);
    const gap = Math.round(22 * s);
    const square = Math.round(24 * s);
    const totalH = stackH + gap + square;
    if ((totalH + 1) * CELL > bottom - top || (WIDE * s + 1) * CELL > right - left) return { key, cols, rows };
    const x0 = Math.round((left + right) / 2 / CELL - (WIDE * s) / 2);
    const y0 = Math.round((top + bottom) / 2 / CELL - totalH / 2);
    // Faint specks around the stack, denser near it, each breathing slowly.
    const specks = [];
    const cx = x0 + (WIDE * s) / 2;
    const cy = y0 + totalH / 2;
    const rx = WIDE * s * 1.1;
    const ry = totalH * 0.95;
    for (let j = 0; j < rows; j++) {
      for (let i = 0; i < cols; i++) {
        const d = ((i - cx) / rx) ** 2 + ((j - cy) / ry) ** 2;
        if (d < 1 && hash(i, j, seed + 8) < 0.012 * (1 - d)) {
          specks.push([j * cols + i, 14 + 22 * hash(i, j, seed + 9), 6.283 * hash(i, j, seed + 10)]);
        }
      }
    }
    return {
      key,
      cols,
      rows,
      pitch,
      x0,
      y0,
      sheet: sheet(s, thick, seed),
      specks,
      square: [Math.round(x0 + 72 * s), y0 + stackH + gap, square],
    };
  }

  function stamp(L, px, pos, cut, gather) {
    const S = L.sheet;
    const ox = L.x0;
    const oy = L.y0 + Math.round(pos * L.pitch);
    const m = PEAK * (1 - 0.11 * pos);
    for (let k = 0; k < S.n; k++) {
      if (S.out[k] < cut || S.gather[k] > gather) continue;
      const x = ox + S.x[k];
      const y = oy + S.y[k];
      if (x < 0 || y < 0 || x >= L.cols || y >= L.rows) continue;
      px[y * L.cols + x] = GRAY[(S.value[k] * m) | 0];
    }
  }

  function draw(L, px, t, seed) {
    const { cols, rows } = L;
    px.fill(GRAY[0]);
    if (!L.sheet) return;

    for (const [at, tone, phase] of L.specks) {
      px[at] = GRAY[(tone * (0.7 + 0.3 * Math.sin(t * 0.5 + phase))) | 0];
    }

    // The small square set apart, as in the mark.
    const [sx, sy, size] = L.square;
    const edge = GRAY[(PEAK * 0.45) | 0];
    for (let n = 0; n <= size; n++) {
      for (const [x, y] of [[sx + n, sy], [sx + n, sy + size], [sx, sy + n], [sx + size, sy + n]]) {
        if (x >= 0 && y >= 0 && x < cols && y < rows) px[y * cols + x] = edge;
      }
    }

    const cycle = Math.floor(t / CYCLE);
    const tau = t - cycle * CYCLE;
    // Any sheet but the top one: that one was just made.
    const idle = 1 + Math.floor(hash(cycle, 0, seed) * (SHEETS - 1));
    const cut = -0.01 + 1.02 * ((tau - REST) / LOOSEN);
    const settle = smooth((tau - SETTLE_AT) / SETTLE);
    const gather = -0.01 + 1.02 * ((tau - ARRIVE_AT) / ARRIVE);

    // Bottom up, so each sheet covers the one below it.
    for (let k = SHEETS - 1; k >= 0; k--) {
      if (k === idle) stamp(L, px, k, cut, 2);
      else stamp(L, px, k < idle ? k + settle : k, -1, 2);
    }
    if (gather > 0) stamp(L, px, 0, -1, gather);

    const oy = L.y0 + idle * L.pitch;
    const m = PEAK * (1 - 0.11 * idle);
    const S = L.sheet;
    for (const p of S.dust) {
      const age = tau - p.at;
      if (age < 0 || age > p.life) continue;
      const x = Math.round(L.x0 + S.x[p.k] + p.vx * age + 1.2 * Math.sin(age * 1.3 + p.phase));
      const y = Math.round(oy + S.y[p.k] + p.vy * age);
      if (x < 0 || y < 0 || x >= cols || y >= rows) continue;
      px[y * cols + x] = GRAY[(S.value[p.k] * m * (1 - age / p.life) ** 1.5) | 0];
    }
  }

  const seed = 1 + Math.floor(Math.random() * 1e6);
  const still = window.matchMedia("(prefers-reduced-motion: reduce)");
  let L = null;
  let image = null;
  let pixels = null;
  let visible = true;
  let raf = 0;
  let timer = 0;
  let last = 0;
  let t = START;

  const render = () => {
    if (!L) return;
    // Reduced motion shows the whole stack at rest.
    draw(L, pixels, still.matches ? 1 : t, seed);
    ctx.putImageData(image, 0, 0);
  };

  // The room above the text or beside it, whichever holds the larger stack.
  // No text box enters it: the room above ends 24px above the text, and the
  // room beside ends 24px above the first box that reaches into it, such as
  // the install command's row.
  const text = canvas.parentElement.querySelector(".hero-content");
  const room = (box, w, h) => {
    const boxes = [...(text ? text.children : [])].map((el) => {
      const r = el.getBoundingClientRect();
      return { top: r.top - box.top, right: r.right - box.left };
    });
    const above = [0, h * 0.06, w, Math.min(h * 0.94, ...boxes.map((b) => b.top - 24))];
    const left = Math.min(w * 0.6, Math.max(0, ...boxes.map((b) => b.right)) + 48);
    const reach = boxes.filter((b) => b.right + 24 > left);
    const beside = [left, h * 0.08, w * 0.97, Math.min(h * 0.92, ...reach.map((b) => b.top - 24))];
    return span(above) > span(beside) ? above : beside;
  };

  const resize = () => {
    const box = canvas.getBoundingClientRect();
    const w = Math.round(box.width);
    const h = Math.round(box.height);
    if (w === 0 || h === 0) return;
    const space = room(box, w, h);
    if (L && L.key === [w, h, ...space.map(Math.round)].join()) return;
    L = layout(w, h, space, seed);
    canvas.width = L.cols;
    canvas.height = L.rows;
    image = ctx.createImageData(L.cols, L.rows);
    pixels = new Uint32Array(image.data.buffer);
    render();
    run();
  };

  // With no stack, nothing moves and no frame is drawn.
  const moving = () => visible && !still.matches && !document.hidden && Boolean(L && L.sheet);

  const tick = (now) => {
    raf = 0;
    if (!moving()) return;
    // A long gap (a stalled tab) moves the stack at most 0.1 s.
    t += last ? Math.min(0.1, (now - last) / 1000) : 0;
    last = now;
    render();
    const tau = t % CYCLE;
    const resting = tau < REST || tau > ARRIVE_AT + ARRIVE;
    // Sleep until 6 ms before the next frame is due, then wait for the
    // display, so frames stay on its beat (at 60, 90 or 120 Hz the next display
    // frame after that is the due one, at 144 Hz the one before it) and
    // skipped ones cost nothing.
    timer = setTimeout(
      () => {
        timer = 0;
        if (moving()) raf = requestAnimationFrame(tick);
      },
      (resting ? 100 : 1000 / 30) - 6,
    );
  };

  const run = () => {
    if (raf || timer || !moving()) return;
    last = 0;
    raf = requestAnimationFrame(tick);
  };

  // The text changes size when the font arrives or the copy button shows.
  const sizes = new ResizeObserver(resize);
  sizes.observe(canvas);
  for (const el of text ? text.children : []) sizes.observe(el);
  new IntersectionObserver((entries) => {
    visible = entries.some((e) => e.isIntersecting);
    run();
  }).observe(canvas);
  document.addEventListener("visibilitychange", run);
  still.addEventListener("change", () => {
    render();
    run();
  });
  resize();
  run();
})();
