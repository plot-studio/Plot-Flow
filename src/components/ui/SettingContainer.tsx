import React, { useEffect, useRef, useState } from "react";
import { Info } from "lucide-react";
import { Tooltip } from "./Tooltip";

interface SettingContainerProps {
  title: string;
  description: string;
  children: React.ReactNode;
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
  layout?: "horizontal" | "stacked";
  disabled?: boolean;
  tooltipPosition?: "top" | "bottom";
}

const InfoTip: React.FC<{
  description: string;
  position: "top" | "bottom";
}> = ({ description, position }) => {
  const [showTooltip, setShowTooltip] = useState(false);
  const tooltipRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!showTooltip) return;
    const handleClickOutside = (event: MouseEvent) => {
      if (
        tooltipRef.current &&
        !tooltipRef.current.contains(event.target as Node)
      ) {
        setShowTooltip(false);
      }
    };
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, [showTooltip]);

  const toggle = () => setShowTooltip((v) => !v);

  return (
    <div
      ref={tooltipRef}
      className="relative flex"
      onMouseEnter={() => setShowTooltip(true)}
      onMouseLeave={() => setShowTooltip(false)}
      onClick={toggle}
    >
      <Info
        className="h-3.5 w-3.5 cursor-help text-text-muted/60 transition-colors duration-150 select-none hover:text-text"
        aria-label={description}
        role="button"
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            toggle();
          }
        }}
      />
      {showTooltip && (
        <Tooltip targetRef={tooltipRef} position={position}>
          <p className="text-sm text-center leading-relaxed">{description}</p>
        </Tooltip>
      )}
    </div>
  );
};

export const SettingContainer: React.FC<SettingContainerProps> = ({
  title,
  description,
  children,
  descriptionMode = "tooltip",
  grouped = false,
  layout = "horizontal",
  disabled = false,
  tooltipPosition = "top",
}) => {
  const frame = grouped ? "" : "pf-card";
  const dim = disabled ? "opacity-50" : "";

  const heading =
    descriptionMode === "tooltip" ? (
      <div className="flex items-center gap-1.5">
        <h3 className={`text-[13.5px] font-normal tracking-[-0.006em] ${dim}`}>
          {title}
        </h3>
        {description && (
          <InfoTip
            description={description}
            position={layout === "stacked" ? "top" : tooltipPosition}
          />
        )}
      </div>
    ) : (
      <div>
        <h3 className={`text-[13.5px] font-normal tracking-[-0.006em] ${dim}`}>
          {title}
        </h3>
        {description && (
          <p
            className={`mt-0.5 text-[12px] leading-snug text-text-muted ${dim}`}
          >
            {description}
          </p>
        )}
      </div>
    );

  if (layout === "stacked") {
    return (
      <div className={`px-4 py-3 ${frame}`}>
        <div className="mb-2">{heading}</div>
        <div className="w-full">{children}</div>
      </div>
    );
  }

  return (
    <div
      className={`flex min-h-11 items-center justify-between gap-6 px-4 py-2.5 ${frame}`}
    >
      <div className="min-w-0 max-w-2/3">{heading}</div>
      <div className="relative shrink-0">{children}</div>
    </div>
  );
};
