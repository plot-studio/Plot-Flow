import React from "react";

interface InputProps extends React.InputHTMLAttributes<HTMLInputElement> {
  variant?: "default" | "compact";
}

export const Input: React.FC<InputProps> = ({
  className = "",
  variant = "default",
  disabled,
  ...props
}) => {
  const baseClasses =
    "text-[13px] bg-text/[0.05] border border-text/[0.08] rounded-lg text-start placeholder:text-text-muted/70 transition-[background-color,border-color,box-shadow] duration-150";

  const interactiveClasses = disabled
    ? "opacity-60 cursor-not-allowed"
    : "hover:border-text/15 focus:outline-none focus:border-logo-primary/60 focus:ring-[3px] focus:ring-logo-primary/20";

  const variantClasses = {
    default: "px-3 py-2",
    compact: "px-2.5 py-1",
  } as const;

  return (
    <input
      className={`${baseClasses} ${variantClasses[variant]} ${interactiveClasses} ${className}`}
      disabled={disabled}
      {...props}
    />
  );
};
