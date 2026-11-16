import React from "react";
import { SettingContainer } from "./SettingContainer";

interface ToggleSwitchProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  isUpdating?: boolean;
  label: string;
  description: string;
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
  tooltipPosition?: "top" | "bottom";
}

export const ToggleSwitch: React.FC<ToggleSwitchProps> = ({
  checked,
  onChange,
  disabled = false,
  isUpdating = false,
  label,
  description,
  descriptionMode = "tooltip",
  grouped = false,
  tooltipPosition = "top",
}) => {
  return (
    <SettingContainer
      title={label}
      description={description}
      descriptionMode={descriptionMode}
      grouped={grouped}
      disabled={disabled}
      tooltipPosition={tooltipPosition}
    >
      <label
        className={`flex items-center ${disabled || isUpdating ? "cursor-not-allowed" : "cursor-pointer"}`}
      >
        <input
          type="checkbox"
          value=""
          className="sr-only peer"
          checked={checked}
          disabled={disabled || isUpdating}
          onChange={(e) => onChange(e.target.checked)}
        />
        <div className="relative h-[22px] w-[38px] rounded-full bg-text/[0.14] shadow-[inset_0_0_0_0.5px_rgba(0,0,0,0.25)] transition-colors duration-200 peer-focus-visible:ring-2 peer-focus-visible:ring-logo-primary/60 peer-focus-visible:ring-offset-2 peer-focus-visible:ring-offset-surface peer-checked:bg-background-ui peer-disabled:opacity-50 after:absolute after:top-[2px] after:start-[2px] after:h-[18px] after:w-[18px] after:rounded-full after:bg-white after:shadow-[0_1px_2px_rgba(0,0,0,0.3),0_2px_6px_rgba(0,0,0,0.15)] after:transition-transform after:duration-[280ms] after:ease-[cubic-bezier(0.25,1,0.5,1)] after:content-[''] peer-checked:after:translate-x-4 rtl:peer-checked:after:-translate-x-4 peer-active:after:scale-x-110"></div>
      </label>
      {isUpdating && (
        <div className="absolute inset-0 flex items-center justify-center">
          <div className="w-4 h-4 border-2 border-logo-primary border-t-transparent rounded-full animate-spin"></div>
        </div>
      )}
    </SettingContainer>
  );
};
