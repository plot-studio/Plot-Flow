import React from "react";

interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?:
    | "primary"
    | "primary-soft"
    | "secondary"
    | "warning"
    | "danger"
    | "danger-ghost"
    | "ghost";
  size?: "sm" | "md" | "lg";
}

/**
 * Quiet, rounded-rect buttons with instant press feedback (scale 0.97 on
 * pointer-down, no hover lift).
 * - primary: solid bronze fill (rare — the one call to action)
 * - secondary: surface with a hairline border
 */
export const Button: React.FC<ButtonProps> = ({
  children,
  className = "",
  variant = "primary",
  size = "md",
  ...props
}) => {
  const baseClasses =
    "inline-flex items-center justify-center gap-1.5 font-medium tracking-[-0.006em] rounded-lg border focus:outline-none whitespace-nowrap transition-[background-color,border-color,color,transform] duration-100 ease-out enabled:active:scale-[0.97] disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer";

  const variantClasses = {
    primary: "pf-accent border-transparent enabled:hover:brightness-[1.06]",
    "primary-soft":
      "text-text bg-logo-primary/12 border-logo-primary/20 hover:bg-logo-primary/20",
    secondary:
      "text-text bg-text/[0.06] border-text/[0.06] hover:bg-text/[0.1]",
    warning:
      "text-text bg-text/[0.06] border-text/[0.06] hover:bg-warning/15 hover:border-warning/50",
    danger: "text-white bg-[#d6453d] border-transparent hover:bg-[#e0514a]",
    "danger-ghost": "text-error border-transparent hover:bg-error/10",
    ghost:
      "text-current border-transparent hover:bg-text/[0.06] hover:text-text",
  };

  const sizeClasses = {
    sm: "h-7 px-3 text-[12.5px]",
    md: "h-8 px-3.5 text-[13px]",
    lg: "h-9 px-4 text-[13.5px]",
  };

  return (
    <button
      className={`${baseClasses} ${variantClasses[variant]} ${sizeClasses[size]} ${className}`}
      {...props}
    >
      {children}
    </button>
  );
};
