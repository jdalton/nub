#!/usr/bin/env node
// Render the cold-start comparison chart from hyperfine's --export-json output,
// in the GitHub-flavored horizontal-bar style of the original cold-start.svg:
// per-row label, a track + a filled bar scaled to the slowest row in the group,
// a ±1σ whisker, and a value label. Two groups stacked vertically (trivial fib,
// import-heavy script).
//
//   node make-chart.mjs --fib results-fib.json --heavy results-heavy.json \
//        --node-version v26.3.1 --out cold-start
//
// Writes <out>.svg. A .png is produced too IF a rasterizer is available
// (`rsvg-convert`, or macOS `qlmanage`); otherwise it prints a one-line note —
// the SVG is the source of truth and renders on GitHub directly.

import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { execFileSync } from "node:child_process";

const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, a, i, arr) => {
    if (a.startsWith("--")) acc.push([a.slice(2), arr[i + 1]]);
    return acc;
  }, []),
);

const FONT =
  "-apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif";

// GitHub palette. The fastest row in each group is highlighted orange; the rest
// are neutral gray; the per-group track is a faint tint of the row color.
const ORANGE = "#fb8500";
const ORANGE_TRACK = "#ffedd5";
const ORANGE_TEXT = "#bd5800";
const GRAY = "#6e7781";
const GRAY_TRACK = "#eaeef2";
const GRAY_TEXT = "#57606a";
const INK = "#1f2328";
const MUTE = "#6e7781";

function load(path) {
  const { results } = JSON.parse(readFileSync(path, "utf8"));
  return results.map((r) => ({
    name: r.command,
    mean: r.mean * 1000,
    sd: r.stddev * 1000,
  }));
}

// Layout constants (px). The track stops well short of the right edge so the
// slowest row's value label (which sits just past the bar end) always fits.
const LABEL_X = 175; // right edge of the row labels
const BAR_X = LABEL_X + 14; // left edge of the bar track
const BAR_W = 360; // track width
const VALUE_W = 175; // reserved space for the "NN.N ms (N.NN× node)" label
const W = BAR_X + BAR_W + VALUE_W;
const ROW_H = 30; // row pitch
const BAR_H = 20;
const GROUP_GAP = 30;
const TITLE_H = 40;

function group(rows, title, subtitle, yStart) {
  const slowest = Math.max(...rows.map((r) => r.mean));
  const fastest = Math.min(...rows.map((r) => r.mean));
  const parts = [];
  parts.push(
    `<text x="14" y="${yStart}" font-size="14" font-weight="600" fill="${INK}" font-family="${FONT}">${title}</text>`,
  );
  parts.push(
    `<text x="${W - 14}" y="${yStart}" text-anchor="end" font-size="12" fill="${MUTE}" font-family="${FONT}">${subtitle}</text>`,
  );
  let y = yStart + 14;
  for (const r of rows) {
    const isFast = r.mean === fastest;
    const bar = isFast ? ORANGE : GRAY;
    const track = isFast ? ORANGE_TRACK : GRAY_TRACK;
    const txt = isFast ? ORANGE_TEXT : GRAY_TEXT;
    const w = Math.max(2, (r.mean / slowest) * BAR_W);
    const cy = y + BAR_H / 2;
    // whisker: mean ± sd, in the same px scale as the bar
    const px = (ms) => BAR_X + (ms / slowest) * BAR_W;
    const lo = px(r.mean - r.sd);
    const hi = px(r.mean + r.sd);
    const factor = (slowest / r.mean).toFixed(2);
    const ratioVsBase = r.vsBase ? ` (${r.vsBase})` : "";
    parts.push(
      `<text x="${LABEL_X}" y="${cy}" text-anchor="end" dominant-baseline="central" font-size="13.5" font-weight="${isFast ? 600 : 400}" fill="${isFast ? INK : GRAY_TEXT}" font-family="${FONT}">${r.name}</text>`,
      `<rect x="${BAR_X}" y="${y}" width="${BAR_W}" height="${BAR_H}" rx="4" fill="${track}"/>`,
      `<rect x="${BAR_X}" y="${y}" width="${w.toFixed(1)}" height="${BAR_H}" rx="4" fill="${bar}"/>`,
      `<line x1="${lo.toFixed(1)}" x2="${hi.toFixed(1)}" y1="${cy}" y2="${cy}" stroke="${INK}" stroke-opacity="0.32" stroke-width="1.5"/>`,
      `<line x1="${lo.toFixed(1)}" x2="${lo.toFixed(1)}" y1="${cy - 4}" y2="${cy + 4}" stroke="${INK}" stroke-opacity="0.32" stroke-width="1.5"/>`,
      `<line x1="${hi.toFixed(1)}" x2="${hi.toFixed(1)}" y1="${cy - 4}" y2="${cy + 4}" stroke="${INK}" stroke-opacity="0.32" stroke-width="1.5"/>`,
      `<text x="${(px(r.mean) + 6).toFixed(1)}" y="${cy}" dominant-baseline="central" font-size="12.5" font-weight="${isFast ? 600 : 500}" fill="${txt}" font-family="${FONT}">${r.mean.toFixed(1)} ms${ratioVsBase}</text>`,
    );
    y += ROW_H;
  }
  return { svg: parts.join("\n  "), endY: y };
}

// Order each group fastest → slowest so the chart reads as a ranking, and tag
// the ×-vs-node factor onto every row.
function prep(rows) {
  const base = rows.find((r) => r.name === "node").mean;
  for (const r of rows) {
    const f = base / r.mean;
    r.vsBase =
      r.name === "node" ? "1.00× node" : `${f.toFixed(2)}× node`;
  }
  return [...rows].sort((a, b) => a.mean - b.mean);
}

const fib = prep(load(args.fib));
const heavy = prep(load(args.heavy));
const nodeVer = args["node-version"] ?? "";
const out = args.out ?? "cold-start";

let y = TITLE_H;
const g1 = group(
  fib,
  "fib (trivial)",
  `macOS arm64 · node ${nodeVer} · hyperfine -N`,
  y,
);
y = g1.endY + GROUP_GAP;
const g2 = group(heavy, "import-heavy (60 modules)", "lower is better", y);
const H = g2.endY + 34;

const svg = `<svg viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="cold start across approaches, fib and import-heavy scripts, lower is better">
  <rect x="0" y="0" width="${W}" height="${H}" rx="8" fill="#ffffff"/>
  <text x="${LABEL_X}" y="24" text-anchor="end" font-size="15" font-weight="600" fill="${INK}" font-family="${FONT}">cold start</text>
  <text x="${BAR_X}" y="24" font-size="13" fill="${MUTE}" font-family="${FONT}">five startup strategies, one script per group — lower is better</text>
  ${g1.svg}
  ${g2.svg}
  <text x="14" y="${H - 12}" font-size="11.5" fill="${MUTE}" font-family="${FONT}">cook = perry native AOT (what nub cook produces)</text>
  <text x="${W - 14}" y="${H - 12}" text-anchor="end" font-size="11.5" fill="${MUTE}" font-family="${FONT}">bars = mean · whiskers = ±1σ · orange = fastest in group</text>
</svg>
`;

writeFileSync(`${out}.svg`, svg);
console.log(`wrote ${out}.svg`);

// Best-effort PNG. Prefer rsvg-convert; fall back to macOS qlmanage; else note.
function which(bin) {
  try {
    return execFileSync("/usr/bin/env", ["which", bin]).toString().trim();
  } catch {
    return "";
  }
}
const png = `${out}.png`;
if (which("rsvg-convert")) {
  execFileSync("rsvg-convert", ["-z", "2", "-o", png, `${out}.svg`]);
  console.log(`wrote ${png} (rsvg-convert)`);
} else if (which("qlmanage")) {
  // qlmanage writes <name>.png next to a thumbnail dir; render at 2x width.
  execFileSync("qlmanage", [
    "-t",
    "-s",
    String(W * 2),
    "-o",
    ".",
    `${out}.svg`,
  ]);
  if (existsSync(`${out}.svg.png`)) {
    execFileSync("mv", [`${out}.svg.png`, png]);
    console.log(`wrote ${png} (qlmanage)`);
  }
} else {
  console.log(
    `note: no SVG rasterizer found (rsvg-convert / qlmanage); ${png} not regenerated. The .svg is the source of truth.`,
  );
}
