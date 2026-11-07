/** The residue mark (docs/assets/logo-dark.svg in whiteheap/residue, same geometry). */
export function Mark({ className }: { className?: string }) {
  return (
    <svg viewBox="26 66 180 182" className={className} aria-hidden focusable="false">
      <g stroke="currentColor" strokeWidth="7" strokeLinejoin="round" strokeLinecap="round" fillRule="evenodd">
        <polygon fill="#8a8a8a" points="164,160 200,124 200,144 164,180" />
        <polygon fill="#b0b0b0" points="164,140 200,104 200,124 164,160" />
        <polygon fill="#d4d4d4" points="164,120 200,84 200,104 164,140" />
        <polygon fill="#a3a3a3" points="32,148 164,160 164,180 32,168" />
        <polygon fill="#c8c8c8" points="32,128 164,140 164,160 32,148" />
        <polygon fill="#e6e6e6" points="32,108 164,120 164,140 32,128" />
        <polygon fill="#f7f7f7" points="32,108 164,120 200,84 68,72" />
        <g fill="none">
          <path d="M68 72 L200 84 L200 144 L164 180 L32 168 L32 108 L68 72 Z" />
          <path d="M32 108 L164 120 L200 84" />
          <path d="M164 120 L164 180" />
          <path d="M32 128 L164 140 L200 104" />
          <path d="M32 148 L164 160 L200 124" />
        </g>
        <rect fill="none" x="100" y="208" width="34" height="34" />
      </g>
    </svg>
  );
}
