import React from "react";

interface BadgeProps {
  children: React.ReactNode;
  variant?: "primary" | "success" | "secondary";
  className?: string;
}

/** Pill badge with a hairline border, as on amicro.dev. */
const Badge: React.FC<BadgeProps> = ({
  children,
  variant = "primary",
  className = "",
}) => {
  const variantClasses = {
    primary: "border-logo-primary/35 bg-logo-primary/12 text-accent-hi",
    success: "border-emerald-500/25 bg-emerald-500/10 text-emerald-400",
    secondary: "border-text/10 bg-text/5 text-text-muted",
  };

  return (
    <span
      className={`inline-flex items-center gap-1 px-2.5 py-0.5 rounded-full border text-[11px] font-medium tracking-tight ${variantClasses[variant]} ${className}`}
    >
      {children}
    </span>
  );
};

export default Badge;
