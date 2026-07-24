/* GitHub mark SVG (official GitHub Invertocat, simplified mono path). */
function GitHubIcon({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="currentColor"
      className={className}
      aria-hidden="true"
    >
      <path d="M12 0C5.37 0 0 5.37 0 12c0 5.31 3.435 9.795 8.205 11.385.6.105.825-.255.825-.57 0-.285-.015-1.23-.015-2.235-3.015.555-3.795-.735-4.035-1.41-.135-.345-.72-1.41-1.23-1.695-.42-.225-1.02-.78-.015-.795.945-.015 1.62.87 1.845 1.23 1.08 1.815 2.805 1.305 3.495.99.105-.78.42-1.305.765-1.605-2.67-.3-5.46-1.335-5.46-5.925 0-1.305.465-2.385 1.23-3.225-.12-.3-.54-1.53.12-3.18 0 0 1.005-.315 3.3 1.23.96-.27 1.98-.405 3-.405s2.04.135 3 .405c2.295-1.56 3.3-1.23 3.3-1.23.66 1.65.24 2.88.12 3.18.765.84 1.23 1.905 1.23 3.225 0 4.605-2.805 5.625-5.475 5.925.435.375.81 1.095.81 2.22 0 1.605-.015 2.895-.015 3.3 0 .315.225.69.825.57A12.02 12.02 0 0 0 24 12c0-6.63-5.37-12-12-12z" />
    </svg>
  );
}

/* Footer node injected into a TOC panel (docs pages and blog posts): a GitHub
   repo link with a handwritten "Leave a star" annotation scribbled below it,
   its little arrow swooping up to the link — the TOC-panel echo of the hero's
   star nudge. The TOC column is a fixed-width sticky element, so (unlike the
   hero, where the pill moves with viewport width) this arrow can be a static
   hand-drawn SVG. The link itself stays an unobtrusive last-entry-of-the-TOC
   affordance. */
export function TocStarNudge({ href }: { href: string }) {
  return (
    <>
      <hr className="mb-4 mt-2 border-fd-foreground/10" />
      <div className="relative ps-3 pb-[72px]">
        <a
          href={href}
          target="_blank"
          rel="noopener noreferrer"
          className="flex items-center gap-1.5 py-1.5 text-sm text-fd-muted-foreground transition-colors hover:text-fd-accent-foreground"
        >
          <GitHubIcon className="size-3.5 shrink-0" />
          <span>nubjs/nub</span>
        </a>
        {/* Handwritten nudge BELOW the link. The arrow sits ENTIRELY below the
            link box and points UP at the link's BOTTOM-MIDDLE. The overlay's
            top-left coincides with the link's top-left; the visible content
            (icon + "nubjs/nub") spans x 0→80, so its bottom-middle is (40,32).
            The tip sits at (40,38) — a ~6px gap under the link — with the shaft
            rising from above the handwriting. A thin stroke + slim head and a
            gentle leftward bow keep it from reading stubby. */}
        <div
          aria-hidden="true"
          className="pointer-events-none absolute left-3 top-0 select-none text-ember/60 dark:text-ember/90"
        >
          <svg
            viewBox="0 0 120 100"
            fill="none"
            className="absolute left-0 top-0 h-[100px] w-[120px]"
          >
            {/* shaft: a gently bowed swoosh from above the label up to the tip
                just below the link's bottom-middle */}
            <path
              d="M52 72 C 40 62, 34 50, 40 38"
              stroke="currentColor"
              strokeWidth="1.7"
              strokeLinecap="round"
            />
            {/* arrowhead, tip at (40,38) — ~6px below the link's bottom-middle — pointing UP */}
            <path
              d="M34 46 L 40 38 L 47 45"
              stroke="currentColor"
              strokeWidth="1.7"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
          <span className="absolute left-[56px] top-[80px] block w-fit -rotate-3 whitespace-nowrap font-[family-name:var(--font-caveat)] text-lg leading-none">
            Leave a star!
          </span>
        </div>
      </div>
    </>
  );
}
