import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface SmartInsertProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

/// Paste only when the cursor is in a text field; otherwise keep the text
/// in memory and on the clipboard so Ctrl+V inserts it later.
export const SmartInsert: React.FC<SmartInsertProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const enabled = getSetting("smart_insert") ?? true;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(nextEnabled) => updateSetting("smart_insert", nextEnabled)}
        isUpdating={isUpdating("smart_insert")}
        label={t("settings.general.smartInsert.title")}
        description={t("settings.general.smartInsert.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);
