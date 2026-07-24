/* ----------------------------------------------------------------------------
   Shared Open Graph / Twitter-card renderer.

   Builds 1200×630 social cards that look like they were lifted off the
   homepage: the `#100f0d` ink ground, a top ember glow, the Encode Sans
   display sans (with the ember "all-in-one" accent), an IBM Plex Mono eyebrow,
   and the `nub.` wordmark with the ember full-stop.

   `next/og` rasterizes a React tree with Satori, which cannot read CSS
   `next/font` faces — it needs raw font buffers. We ship the exact Encode Sans
   weights the site uses in `public/` and the IBM Plex Mono weights under
   `src/lib/og-fonts/`, and load them at module scope (cached across renders).
   Everything here is pure layout + inline styles, the only shape Satori
   supports. (Encode Sans has no italic; the homepage accent is carried by the
   ember colour plus a heavier weight rather than a slant.)
---------------------------------------------------------------------------- */
import { ImageResponse } from 'next/og';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { ReactElement } from 'react';

export const OG_SIZE = { width: 1200, height: 630 };
export const OG_CONTENT_TYPE = 'image/png';

/* Homepage palette (global.css). Kept literal so Satori — which has no access
   to CSS custom properties — renders the same ink/ember the site does. */
const INK = '#100f0d';
const FOREGROUND = '#ece6d8';
const MUTED = '#aba297';
const EMBER = '#ff5d3b';
const BORDER = '#2a2620';

const FONT_DIR = join(process.cwd(), 'src', 'lib', 'og-fonts');
const PUBLIC_DIR = join(process.cwd(), 'public');
const font = (file: string) => readFileSync(join(FONT_DIR, file));
const publicFont = (file: string) => readFileSync(join(PUBLIC_DIR, file));

/* Loaded once per server process, reused for every card. Encode Sans rides in
   `public/`; IBM Plex Mono stays under og-fonts. */
const fonts = [
  { name: 'Encode Sans', data: publicFont('encode-sans-regular.ttf'), weight: 400 as const, style: 'normal' as const },
  { name: 'Encode Sans', data: publicFont('encode-sans-medium.ttf'), weight: 500 as const, style: 'normal' as const },
  { name: 'Encode Sans', data: publicFont('encode-sans-semibold.ttf'), weight: 600 as const, style: 'normal' as const },
  { name: 'IBM Plex Mono', data: font('plexmono-regular.ttf'), weight: 400 as const, style: 'normal' as const },
  { name: 'IBM Plex Mono', data: font('plexmono-medium.ttf'), weight: 500 as const, style: 'normal' as const },
];

/* The `nub.` wordmark — Encode Sans display sans, ember full-stop. Matches the
   footer treatment on the homepage. */
function Wordmark({ size }: { size: number }) {
  return (
    <div
      style={{
        display: 'flex',
        fontFamily: 'Encode Sans',
        fontWeight: 500,
        fontSize: size,
        letterSpacing: '-0.015em',
        color: FOREGROUND,
        lineHeight: 1,
      }}
    >
      nub<span style={{ color: EMBER }}>.</span>
    </div>
  );
}

type CardProps = {
  /* Mono uppercase eyebrow, e.g. "DOCUMENTATION" or a command like "nub run".
     Optional — the home card drops it (the tagline carries the message). */
  eyebrow?: string;
  /* Headline — the page title, or the homepage tagline. */
  title: ReactElement | string;
};

/* Size the headline by length: short titles read large, long ones step down so
   they still fit the card with clear space above the footer wordmark (the
   longest bucket wraps to three lines). Sized generously — these cards render
   small in a feed, so the title has to carry from a thumbnail. Element titles
   (the home tagline) use a fixed size since their length can't be measured
   here. */
function titleFontSize(title: ReactElement | string): string {
  if (typeof title !== 'string') return '108px';
  if (title.length <= 16) return '132px';
  if (title.length <= 24) return '112px';
  if (title.length <= 40) return '92px';
  if (title.length <= 58) return '80px';
  return '72px';
}

/* The card body, shared by the home and per-page cards. The ember glow, ink
   ground, eyebrow, and sans headline are pinned top-left (the title sits
   directly under the eyebrow); the footer wordmark anchors the bottom. All
   positioned with the absolute-size geometry Satori needs. */
function Card({ eyebrow, title }: CardProps) {
  return (
    <div
      style={{
        width: '100%',
        height: '100%',
        display: 'flex',
        flexDirection: 'column',
        justifyContent: 'space-between',
        position: 'relative',
        backgroundColor: INK,
        padding: '80px',
        fontFamily: 'Encode Sans',
      }}
    >
      {/* Top ember glow — mirrors the hero's radial-gradient wash. */}
      <div
        style={{
          position: 'absolute',
          top: '-340px',
          left: '50%',
          marginLeft: '-440px',
          width: '880px',
          height: '660px',
          borderRadius: '9999px',
          background:
            'radial-gradient(closest-side, rgba(255,93,59,0.28), rgba(255,93,59,0.06) 60%, transparent 75%)',
        }}
      />
      {/* Hairline frame, set in from the bleed — gives the card an editorial
          border without competing with the headline. */}
      <div
        style={{
          position: 'absolute',
          inset: '28px',
          border: `1px solid ${BORDER}`,
          borderRadius: '20px',
        }}
      />

      {/* Top-left block: mono eyebrow, then the page title set large directly
          beneath it. No subtitle — the title carries the card, and at
          feed-thumbnail size a big title reads where a paragraph wouldn't. */}
      <div
        style={{
          display: 'flex',
          flexDirection: 'column',
          position: 'relative',
        }}
      >
        {eyebrow ? (
          <div
            style={{
              display: 'flex',
              alignItems: 'center',
              fontFamily: 'IBM Plex Mono',
              fontWeight: 500,
              fontSize: '30px',
              letterSpacing: '0.18em',
              textTransform: 'uppercase',
              color: EMBER,
              marginBottom: '32px',
            }}
          >
            {eyebrow}
          </div>
        ) : null}
        <div
          style={{
            display: 'flex',
            flexWrap: 'wrap',
            fontFamily: 'Encode Sans',
            fontWeight: 500,
            fontSize: titleFontSize(title),
            lineHeight: 1.02,
            letterSpacing: '-0.02em',
            color: FOREGROUND,
            maxWidth: '1040px',
          }}
        >
          {title}
        </div>
      </div>

      {/* Footer: wordmark left, domain right. */}
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'space-between',
          position: 'relative',
        }}
      >
        <Wordmark size={48} />
        <div
          style={{
            display: 'flex',
            fontFamily: 'IBM Plex Mono',
            fontWeight: 400,
            // Sized so the mono x-height matches the 48px Encode Sans wordmark's
            // x-height (Plex Mono 0.516/em vs Encode 0.543/em → ~48px), so the
            // domain reads at the same optical size as "nub." across the footer.
            fontSize: '48px',
            letterSpacing: '0.02em',
            color: MUTED,
          }}
        >
          nubjs.com
        </div>
      </div>
    </div>
  );
}

/* Render a card to a PNG ImageResponse. */
export function renderOgImage(props: CardProps) {
  return new ImageResponse(<Card {...props} />, { ...OG_SIZE, fonts });
}

/* The homepage card: the punchy tagline, "all-in-one" set in ember + a heavier
   weight to match the homepage accent treatment (Encode Sans has no italic, so
   the slant the homepage once used is carried by colour + weight here). No
   eyebrow (it just restated the tagline) — the tagline carries it. */
export function renderHomeOgImage() {
  return renderOgImage({
    title: (
      <div style={{ display: 'flex', flexWrap: 'wrap' }}>
        <span style={{ display: 'flex' }}>The&nbsp;</span>
        <span style={{ display: 'flex', fontWeight: 600, color: EMBER }}>all-in-one</span>
        <span style={{ display: 'flex' }}>&nbsp;Node.js toolkit</span>
      </div>
    ),
  });
}
