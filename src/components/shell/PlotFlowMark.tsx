import React from "react";

/**
 * Plot Flow glyph: a graphite tile with a bronze voice wave through it.
 * Pure SVG driven by the theme tokens, so it follows light/dark.
 */
export const PlotFlowMark: React.FC<{ size?: number; className?: string }> = ({
  size = 20,
  className = "",
}) => (
  <svg
    width={size}
    height={size}
    viewBox="0 0 24 24"
    className={className}
    aria-hidden="true"
  >
    <rect
      x="0.5"
      y="0.5"
      width="23"
      height="23"
      rx="6.5"
      style={{
        fill: "var(--color-surface-2)",
        stroke: "color-mix(in srgb, var(--color-text) 12%, transparent)",
      }}
    />
    <g
      fill="none"
      strokeWidth="2.1"
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ stroke: "var(--color-logo-primary)" }}
    >
      <path d="M6 12v0.01" />
      <path d="M9 9.5v5" />
      <path d="M12 6.5v11" />
      <path d="M15 9v6" />
      <path d="M18 11v2" />
    </g>
  </svg>
);
