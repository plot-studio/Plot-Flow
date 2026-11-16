import React from "react";

interface TextareaProps extends React.TextareaHTMLAttributes<HTMLTextAreaElement> {
  variant?: "default" | "compact";
}

export const Textarea: React.FC<TextareaProps> = ({
  className = "",
  variant = "default",
  ...props
}) => {
  const baseClasses =
    "text-[13px] bg-text/[0.05] border border-text/[0.08] rounded-lg text-start placeholder:text-text-muted/70 transition-[background-color,border-color,box-shadow] duration-150 hover:border-text/15 focus:outline-none focus:border-logo-primary/60 focus:ring-[3px] focus:ring-logo-primary/20 resize-y";

  const variantClasses = {
    default: "px-3 py-2 min-h-[100px]",
    compact: "px-2.5 py-1 min-h-[80px]",
  };

  return (
    <textarea
      className={`${baseClasses} ${variantClasses[variant]} ${className}`}
      {...props}
    />
  );
};
