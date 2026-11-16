import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface LiveInsertProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

/// Paste every phrase as soon as the speaker pauses, instead of the whole
/// dictation when the recording stops.
export const LiveInsert: React.FC<LiveInsertProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const enabled = getSetting("live_insert") ?? true;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(nextEnabled) => updateSetting("live_insert", nextEnabled)}
        isUpdating={isUpdating("live_insert")}
        label={t("settings.general.liveInsert.title")}
        description={t("settings.general.liveInsert.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);
