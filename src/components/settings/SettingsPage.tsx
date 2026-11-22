import { useState } from "react";
import { useTranslation } from "react-i18next";
import { type } from "@tauri-apps/plugin-os";
import { AnimatePresence, motion } from "motion/react";
import { ChevronRight, Cpu } from "lucide-react";
import { PRESS_SCALE, SPRING, SPRING_SNAPPY } from "@/lib/motion";
import { useSettings } from "@/hooks/useSettings";
import { SettingsGroup } from "../ui/SettingsGroup";
import { ShortcutInput } from "./ShortcutInput";
import { MenuKeyShortcut } from "./MenuKeyShortcut";
import { ShortcutActivationSetting } from "./ShortcutActivation";
import { LiveInsert } from "./LiveInsert";
import { SmartInsert } from "./SmartInsert";
import { MicrophoneSelector } from "./MicrophoneSelector";
import { AutoMicGain } from "./AutoMicGain";
import { AutostartToggle } from "./AutostartToggle";
import { FillerWordRemoval } from "./FillerWordRemoval";
import { CustomWords } from "./CustomWords";
import { AlwaysOnMicrophone } from "./AlwaysOnMicrophone";
import { VoiceActivityDetection } from "./VoiceActivityDetection";
import { AudioFeedback } from "./AudioFeedback";
import { SoundPicker } from "./SoundPicker";
import { OutputDeviceSelector } from "./OutputDeviceSelector";
import { VolumeSlider } from "./VolumeSlider";
import { MuteWhileRecording } from "./MuteWhileRecording";
import { ShowOverlay } from "./ShowOverlay";
import { AutoSubmit } from "./AutoSubmit";
import { AppendTrailingSpace } from "./AppendTrailingSpace";
import { StartHidden } from "./StartHidden";
import { ShowTrayIcon } from "./ShowTrayIcon";
import { ThemeSelector } from "./ThemeSelector";
import { HistoryLimit } from "./HistoryLimit";
import { RecordingRetentionPeriodSelector } from "./RecordingRetentionPeriod";

const MORE_OPEN_KEY = "plotflow.settings.moreOpen";

const readMoreOpen = () => {
  try {
    return localStorage.getItem(MORE_OPEN_KEY) === "1";
  } catch {
    return false;
  }
};

export function SettingsPage() {
  const { t } = useTranslation();
  const { audioFeedbackEnabled } = useSettings();
  const isLinux = type() === "linux";
  const [moreOpen, setMoreOpen] = useState(readMoreOpen);
  const [animating, setAnimating] = useState(false);

  const toggleMore = () => {
    setMoreOpen((open) => {
      const next = !open;
      try {
        localStorage.setItem(MORE_OPEN_KEY, next ? "1" : "0");
      } catch {
        // ignore
      }
      return next;
    });
  };

  return (
    <div className="w-full max-w-[600px] space-y-6">
      <header className="px-1">
        <h1 className="pf-title">{t("settings.page.title")}</h1>
      </header>

      {/* Основные — always open, descriptions shown inline */}
      <SettingsGroup title={t("settings.page.basic")}>
        <ShortcutInput
          shortcutId="transcribe"
          descriptionMode="inline"
          grouped
        />
        {!isLinux && <MenuKeyShortcut descriptionMode="inline" grouped />}
        <ShortcutActivationSetting descriptionMode="inline" grouped />
        <LiveInsert descriptionMode="inline" grouped />
        <SmartInsert descriptionMode="inline" grouped />
        <MicrophoneSelector descriptionMode="inline" grouped />
        <AutoMicGain descriptionMode="inline" grouped />
        <AutostartToggle descriptionMode="inline" grouped />
      </SettingsGroup>

      {/* Дополнительно — collapsed by default */}
      <section>
        <motion.button
          type="button"
          onClick={toggleMore}
          aria-expanded={moreOpen}
          whileTap={{ scale: PRESS_SCALE }}
          transition={SPRING_SNAPPY}
          className="pf-card group flex min-h-11 w-full cursor-pointer items-center justify-between gap-4 px-4 py-2.5 text-start transition-colors duration-150 hover:bg-surface-2"
        >
          <span className="min-w-0">
            <span className="block text-[13.5px] tracking-[-0.006em]">
              {t("settings.page.more")}
            </span>
            <span className="block truncate text-[12px] text-text-muted">
              {t("settings.page.moreHint")}
            </span>
          </span>
          <motion.span
            animate={{ rotate: moreOpen ? 90 : 0 }}
            transition={SPRING_SNAPPY}
            className="flex text-text-muted group-hover:text-text"
          >
            <ChevronRight className="h-4 w-4" />
          </motion.span>
        </motion.button>

        <AnimatePresence initial={false}>
          {moreOpen && (
            <motion.div
              key="more"
              initial={{ height: 0, opacity: 0 }}
              animate={{ height: "auto", opacity: 1 }}
              exit={{ height: 0, opacity: 0 }}
              transition={SPRING}
              onAnimationStart={() => setAnimating(true)}
              onAnimationComplete={() => setAnimating(false)}
              // Clip only while animating: open dropdown menus must be able
              // to overflow the section once it has settled.
              className={animating ? "overflow-hidden" : "overflow-visible"}
            >
              <div className="space-y-6 pt-6">
                <SettingsGroup title={t("settings.page.groups.recording")}>
                  <FillerWordRemoval descriptionMode="tooltip" grouped />
                  <CustomWords descriptionMode="tooltip" grouped />
                  <AlwaysOnMicrophone descriptionMode="tooltip" grouped />
                  <VoiceActivityDetection descriptionMode="tooltip" grouped />
                  {/* Cancel shortcut stays hidden on Linux (dynamic shortcut instability). */}
                  {!isLinux && (
                    <ShortcutInput
                      shortcutId="cancel"
                      descriptionMode="tooltip"
                      grouped
                    />
                  )}
                </SettingsGroup>

                <SettingsGroup title={t("settings.page.groups.sounds")}>
                  <AudioFeedback descriptionMode="tooltip" grouped />
                  {audioFeedbackEnabled && (
                    <>
                      <SoundPicker
                        label={t("settings.debug.soundTheme.label")}
                        description={t("settings.debug.soundTheme.description")}
                      />
                      <OutputDeviceSelector descriptionMode="tooltip" grouped />
                      <VolumeSlider />
                    </>
                  )}
                  <MuteWhileRecording descriptionMode="tooltip" grouped />
                  <ShowOverlay descriptionMode="tooltip" grouped />
                </SettingsGroup>

                <SettingsGroup title={t("settings.page.groups.paste")}>
                  <AutoSubmit descriptionMode="tooltip" grouped />
                  <AppendTrailingSpace descriptionMode="tooltip" grouped />
                </SettingsGroup>

                <SettingsGroup title={t("settings.page.groups.app")}>
                  <StartHidden descriptionMode="tooltip" grouped />
                  <ShowTrayIcon descriptionMode="tooltip" grouped />
                  <ThemeSelector descriptionMode="tooltip" grouped />
                </SettingsGroup>

                <SettingsGroup title={t("settings.page.groups.history")}>
                  <HistoryLimit descriptionMode="tooltip" grouped />
                  <RecordingRetentionPeriodSelector
                    descriptionMode="tooltip"
                    grouped
                  />
                </SettingsGroup>

                <p className="flex items-center gap-2 px-1 text-xs text-text-muted">
                  <Cpu className="h-3.5 w-3.5" strokeWidth={1.8} />
                  {t("settings.page.model")}
                </p>
              </div>
            </motion.div>
          )}
        </AnimatePresence>
      </section>
    </div>
  );
}
