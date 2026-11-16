import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AnimatePresence, motion } from "motion/react";
import { Check, ChevronDown } from "lucide-react";
import { SPRING_SNAPPY } from "@/lib/motion";

export interface DropdownOption {
  value: string;
  label: string;
  description?: string;
  disabled?: boolean;
}

interface DropdownProps {
  options: DropdownOption[];
  className?: string;
  menuClassName?: string;
  selectedValue: string | null;
  onSelect: (value: string) => void;
  placeholder?: string;
  disabled?: boolean;
  onRefresh?: () => void;
}

export const Dropdown: React.FC<DropdownProps> = ({
  options,
  selectedValue,
  onSelect,
  className = "",
  menuClassName,
  placeholder = "Select an option...",
  disabled = false,
  onRefresh,
}) => {
  const { t } = useTranslation();
  const [isOpen, setIsOpen] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (
        dropdownRef.current &&
        !dropdownRef.current.contains(event.target as Node)
      ) {
        setIsOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, []);

  useEffect(() => {
    if (!isOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setIsOpen(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [isOpen]);

  const selectedOption = options.find(
    (option) => option.value === selectedValue,
  );

  const handleSelect = (value: string) => {
    onSelect(value);
    setIsOpen(false);
  };

  const handleToggle = () => {
    if (disabled) return;
    if (!isOpen && onRefresh) onRefresh();
    setIsOpen(!isOpen);
  };

  return (
    <div className={`relative ${className}`} ref={dropdownRef}>
      <button
        type="button"
        aria-expanded={isOpen}
        className={`h-8 px-3 text-[13px] bg-text/[0.06] border rounded-lg min-w-[200px] w-full text-start grid grid-cols-[1fr_auto] gap-2 items-center transition-[border-color,background-color,transform] duration-100 ${
          isOpen ? "border-text/15" : "border-text/[0.06]"
        } ${
          disabled
            ? "opacity-50 cursor-not-allowed"
            : "cursor-pointer hover:bg-text/[0.09] active:scale-[0.98]"
        }`}
        onClick={handleToggle}
        disabled={disabled}
      >
        <span className="truncate">{selectedOption?.label || placeholder}</span>
        <ChevronDown
          className={`h-4 w-4 text-text-muted transition-transform duration-200 ${isOpen ? "rotate-180" : ""}`}
        />
      </button>
      <AnimatePresence>
        {isOpen && !disabled && (
          <motion.div
            initial={{ opacity: 0, y: -4, scale: 0.97 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -4, scale: 0.97 }}
            transition={SPRING_SNAPPY}
            className={`absolute top-full mt-1.5 origin-top bg-surface-2/90 backdrop-blur-xl backdrop-saturate-150 border border-text/10 rounded-xl shadow-[inset_0_1px_0_rgba(255,255,255,0.04),0_16px_40px_-12px_rgba(0,0,0,0.6)] z-50 max-h-60 overflow-y-auto p-1 ${
              menuClassName ?? "left-0 right-0"
            }`}
          >
            {options.length === 0 ? (
              <div className="px-3 py-1.5 text-sm text-text-muted">
                {t("common.noOptionsFound")}
              </div>
            ) : (
              options.map((option) => {
                const selected = selectedValue === option.value;
                return (
                  <button
                    key={option.value}
                    type="button"
                    className={`w-full rounded-md text-[13px] text-start transition-colors duration-150 grid grid-cols-[1fr_auto] items-center gap-2 ${
                      option.description ? "px-3 py-2" : "px-3 py-1.5"
                    } ${
                      selected
                        ? "bg-text/[0.07] text-text"
                        : "hover:bg-text/[0.06]"
                    } ${option.disabled ? "opacity-50 cursor-not-allowed" : "cursor-pointer"}`}
                    onClick={() => handleSelect(option.value)}
                    disabled={option.disabled}
                  >
                    <span className="min-w-0">
                      <span
                        className={`block whitespace-normal break-words ${
                          option.description || selected ? "font-medium" : ""
                        }`}
                      >
                        {option.label}
                      </span>
                      {option.description && (
                        <span className="mt-0.5 block whitespace-normal text-xs font-normal leading-snug text-text-muted">
                          {option.description}
                        </span>
                      )}
                    </span>
                    {selected && (
                      <Check className="h-3.5 w-3.5 text-logo-primary" />
                    )}
                  </button>
                );
              })
            )}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
};
