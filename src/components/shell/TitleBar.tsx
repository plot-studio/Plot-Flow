import React from "react";
import { useTranslation } from "react-i18next";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Minus, X } from "lucide-react";
import { PlotFlowMark } from "./PlotFlowMark";

export const TITLE_BAR_HEIGHT = 36;

const minimize = () => {
  void getCurrentWindow()
    .minimize()
    .catch((e) => console.warn("minimize failed:", e));
};

// Same path as the old native close button: `close()` raises CloseRequested,
// whose Rust handler (lib.rs on_window_event) prevents the close and hides the
// window to the tray. If the request itself is refused, hide directly.
const closeToTray = () => {
  const win = getCurrentWindow();
  void win.close().catch((e) => {
    console.warn("close failed, hiding instead:", e);
    void win.hide().catch(() => undefined);
  });
};

/** Windows-style caption button: full bar height, no chrome until hovered. */
const WindowButton: React.FC<{
  label: string;
  onClick: () => void;
  danger?: boolean;
  children: React.ReactNode;
}> = ({ label, onClick, danger, children }) => (
  <button
    type="button"
    onClick={onClick}
    aria-label={label}
    title={label}
    className={`flex h-full w-[46px] cursor-pointer items-center justify-center text-text-muted transition-colors duration-100 ${
      danger
        ? "hover:bg-[#c42b1c] hover:text-white active:bg-[#b02618]"
        : "hover:bg-text/[0.06] hover:text-text active:bg-text/[0.04]"
    }`}
  >
    {children}
  </button>
);

/**
 * Custom title bar for the frameless main window. Sits on the window
 * material, quiet: a small glyph and name, then the caption buttons. The
 * whole strip is a drag region except the buttons.
 */
export const TitleBar: React.FC = () => {
  const { t } = useTranslation();
  return (
    <header
      data-tauri-drag-region
      style={{ height: TITLE_BAR_HEIGHT }}
      className="relative z-40 flex shrink-0 items-center justify-between ps-4 select-none"
    >
      <div
        data-tauri-drag-region
        className="flex items-center gap-2 text-[12px] font-medium tracking-[-0.005em] text-text-muted"
      >
        <PlotFlowMark size={14} />
        <span data-tauri-drag-region>{t("shell.appName")}</span>
      </div>
      <div data-tauri-drag-region className="h-full flex-1" />
      <div className="flex h-full items-stretch">
        <WindowButton label={t("shell.minimize")} onClick={minimize}>
          <Minus className="h-3.5 w-3.5" strokeWidth={1.75} />
        </WindowButton>
        <WindowButton label={t("shell.close")} onClick={closeToTray} danger>
          <X className="h-3.5 w-3.5" strokeWidth={1.75} />
        </WindowButton>
      </div>
    </header>
  );
};
