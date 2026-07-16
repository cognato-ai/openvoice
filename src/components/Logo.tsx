/** Wave mark only — no plate. Inherits `currentColor` for light/dark. */
export default function Logo({
  size = 28,
  className = "",
}: {
  size?: number;
  className?: string;
}) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 32 32"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden
    >
      <rect x="5" y="13" width="3" height="6" rx="1.5" fill="currentColor" />
      <rect x="10.5" y="9" width="3" height="14" rx="1.5" fill="currentColor" />
      <rect x="16" y="5" width="3" height="22" rx="1.5" fill="currentColor" />
      <rect x="21.5" y="9" width="3" height="14" rx="1.5" fill="currentColor" />
      <rect x="27" y="13" width="3" height="6" rx="1.5" fill="currentColor" />
    </svg>
  );
}
