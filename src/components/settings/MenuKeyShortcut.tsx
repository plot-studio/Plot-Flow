import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { useSettings } from "../../hooks/useSettings";
import { SettingContainer } from "../ui/SettingContainer";
import { Button } from "../ui/Button";

/** The context-menu key as stored by the hotkey engine. */
const MENU_KEY = "contextmenu";

interface MenuKeyShortcutProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

/**
 * One click to start dictation with the context-menu key (right of the space
 * bar). The app catches the key itself, so no key-remapping tool is needed.
 */
export const MenuKeyShortcut: React.FC<MenuKeyShortcutProps> = ({
  descriptionMode = "inline",
  grouped = false,
}) => {
  const { t } = useTranslation();
  const { settings, updateBinding } = useSettings();
  const [busy, setBusy] = useState(false);
  const current = settings?.bindings?.transcribe?.current_binding ?? "";
  const active = current === MENU_KEY;

  const useMenuKey = async () => {
    setBusy(true);
    try {
      await updateBinding("transcribe", MENU_KEY);
      toast.success(t("settings.general.menuKey.done"));
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <SettingContainer
      title={t("settings.general.menuKey.title")}
      description={t("settings.general.menuKey.description")}
      descriptionMode={descriptionMode}
      grouped={grouped}
    >
      <Button
        variant={active ? "secondary" : "primary"}
        size="sm"
        disabled={active || busy}
        onClick={() => void useMenuKey()}
      >
        {active
          ? t("settings.general.menuKey.active")
          : t("settings.general.menuKey.action")}
      </Button>
    </SettingContainer>
  );
};
