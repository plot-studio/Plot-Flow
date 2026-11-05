import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface AutoMicGainProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

/// Raise the microphone level to maximum while recording (restored afterwards)
/// and boost very quiet recordings in software before recognition.
export const AutoMicGain: React.FC<AutoMicGainProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const enabled = getSetting("auto_mic_gain") ?? true;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(nextEnabled) => updateSetting("auto_mic_gain", nextEnabled)}
        isUpdating={isUpdating("auto_mic_gain")}
        label={t("settings.advanced.autoMicGain.title")}
        description={t("settings.advanced.autoMicGain.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);
