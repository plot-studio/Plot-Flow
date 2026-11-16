import React from "react";
import SelectComponent from "react-select";
import CreatableSelect from "react-select/creatable";
import type {
  ActionMeta,
  Props as ReactSelectProps,
  SingleValue,
  StylesConfig,
} from "react-select";

export type SelectOption = {
  value: string;
  label: string;
  isDisabled?: boolean;
};

type BaseProps = {
  value: string | null;
  options: SelectOption[];
  placeholder?: string;
  disabled?: boolean;
  isLoading?: boolean;
  isClearable?: boolean;
  onChange: (value: string | null, action: ActionMeta<SelectOption>) => void;
  onBlur?: () => void;
  className?: string;
  formatCreateLabel?: (input: string) => string;
};

type CreatableProps = {
  isCreatable: true;
  onCreateOption: (value: string) => void;
};

type NonCreatableProps = {
  isCreatable?: false;
  onCreateOption?: never;
};

export type SelectProps = BaseProps & (CreatableProps | NonCreatableProps);

const hairline = "color-mix(in srgb, var(--color-text) 10%, transparent)";
const hairlineHover = "color-mix(in srgb, var(--color-text) 20%, transparent)";
const hoverBackground = "color-mix(in srgb, var(--color-text) 5%, transparent)";
const selectedBackground =
  "color-mix(in srgb, var(--color-logo-primary) 15%, transparent)";
const muted = "var(--color-text-muted)";

const selectStyles: StylesConfig<SelectOption, false> = {
  control: (base, state) => ({
    ...base,
    minHeight: 32,
    borderRadius: 8,
    borderColor: state.isFocused
      ? "color-mix(in srgb, var(--color-logo-primary) 70%, transparent)"
      : hairline,
    boxShadow: state.isFocused
      ? "0 0 0 3px color-mix(in srgb, var(--color-logo-primary) 20%, transparent)"
      : "none",
    backgroundColor: "color-mix(in srgb, var(--color-text) 5%, transparent)",
    fontSize: 13,
    color: "var(--color-text)",
    transition: "border-color 150ms ease, box-shadow 150ms ease",
    ":hover": {
      borderColor: state.isFocused
        ? "color-mix(in srgb, var(--color-logo-primary) 70%, transparent)"
        : hairlineHover,
    },
  }),
  valueContainer: (base) => ({
    ...base,
    paddingInline: 12,
    paddingBlock: 2,
  }),
  input: (base) => ({
    ...base,
    color: "var(--color-text)",
  }),
  singleValue: (base) => ({
    ...base,
    color: "var(--color-text)",
  }),
  indicatorSeparator: () => ({ display: "none" }),
  dropdownIndicator: (base, state) => ({
    ...base,
    color: state.isFocused ? "var(--color-logo-primary)" : muted,
    ":hover": {
      color: "var(--color-logo-primary)",
    },
  }),
  clearIndicator: (base) => ({
    ...base,
    color: muted,
    ":hover": {
      color: "var(--color-logo-primary)",
    },
  }),
  menu: (provided) => ({
    ...provided,
    zIndex: 30,
    padding: 4,
    borderRadius: 12,
    backgroundColor: "var(--color-surface-2)",
    color: "var(--color-text)",
    border: `1px solid ${hairline}`,
    boxShadow: "0 16px 40px -12px rgba(0, 0, 0, 0.6)",
  }),
  option: (base, state) => ({
    ...base,
    borderRadius: 10,
    backgroundColor: state.isSelected
      ? selectedBackground
      : state.isFocused
        ? hoverBackground
        : "transparent",
    color: "var(--color-text)",
    cursor: state.isDisabled ? "not-allowed" : "pointer",
    opacity: state.isDisabled ? 0.5 : 1,
  }),
  placeholder: (base) => ({
    ...base,
    color: muted,
  }),
};

export const Select: React.FC<SelectProps> = React.memo(
  ({
    value,
    options,
    placeholder,
    disabled,
    isLoading,
    isClearable = true,
    onChange,
    onBlur,
    className = "",
    isCreatable,
    formatCreateLabel,
    onCreateOption,
  }) => {
    const selectValue = React.useMemo(() => {
      if (!value) return null;
      const existing = options.find((option) => option.value === value);
      if (existing) return existing;
      return { value, label: value, isDisabled: false };
    }, [value, options]);

    const handleChange = (
      option: SingleValue<SelectOption>,
      action: ActionMeta<SelectOption>,
    ) => {
      onChange(option?.value ?? null, action);
    };

    const sharedProps: Partial<ReactSelectProps<SelectOption, false>> = {
      className,
      classNamePrefix: "app-select",
      value: selectValue,
      options,
      onChange: handleChange,
      placeholder,
      isDisabled: disabled,
      isLoading,
      onBlur,
      isClearable,
      styles: selectStyles,
    };

    if (isCreatable) {
      return (
        <CreatableSelect<SelectOption, false>
          {...sharedProps}
          onCreateOption={onCreateOption}
          formatCreateLabel={formatCreateLabel}
        />
      );
    }

    return <SelectComponent<SelectOption, false> {...sharedProps} />;
  },
);

Select.displayName = "Select";
