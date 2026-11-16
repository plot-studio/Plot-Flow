import React from "react";
import { useTranslation } from "react-i18next";
import { Mic } from "lucide-react";
import { useSettings } from "@/hooks/useSettings";
import { useOsType } from "@/hooks/useOsType";
import { formatKeyCombination } from "@/lib/utils/keyboard";
import { HistorySettings } from "../settings/history/HistorySettings";

/** Live status: the dictation hotkey as keycaps and the active microphone. */
const LiveStatus: React.FC = () => {
  const { t } = useTranslation();
  const { settings, getSetting } = useSettings();
  const osType = useOsType();

  const binding = settings?.bindings?.transcribe?.current_binding ?? "";
  const hotkey = binding ? formatKeyCombination(binding, osType) : "";
  const mic = getSetting("selected_microphone");
  const micName =
    !mic || mic.toLowerCase() === "default" ? t("home.micDefault") : mic;

  return (
    <div className="flex items-center gap-3 pe-1.5 text-[12px] text-text-muted">
      {hotkey && (
        <span
          className="inline-flex items-center gap-1"
          title={t("home.hotkeyTitle")}
        >
          {hotkey.split(" + ").map((key, i) => (
            <kbd key={`${key}-${i}`} className="pf-kbd font-sans">
              {key}
            </kbd>
          ))}
        </span>
      )}
      <span
        className="inline-flex max-w-[180px] items-center gap-1.5"
        title={`${t("home.micTitle")}: ${micName}`}
      >
        <Mic className="h-3.5 w-3.5 shrink-0" strokeWidth={1.8} />
        <span className="truncate">{micName}</span>
      </span>
    </div>
  );
};

/**
 * Home: content first. The recent dictations are the page; the header only
 * carries a title and a tiny live status.
 */
export const HomePage: React.FC = () => (
  <div className="flex w-full max-w-[680px] flex-col">
    <HistorySettings headerAccessory={<LiveStatus />} />
  </div>
);
