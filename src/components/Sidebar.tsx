import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "motion/react";
import { getVersion } from "@tauri-apps/api/app";
import {
  ChartNoAxesCombined,
  House,
  Settings2,
  Users,
  type LucideIcon,
} from "lucide-react";

import { SettingsPage, StatisticsSettings } from "./settings";
import { MeetingPage } from "./meeting";
import { HomePage } from "./home/HomePage";
import type { AppSection } from "@/stores/navigationStore";
import { PRESS_SCALE, SPRING_SNAPPY } from "@/lib/motion";

export type SidebarSection = AppSection;

interface SectionConfig {
  labelKey: string;
  icon: LucideIcon;
  component: React.ComponentType;
}

export const SECTIONS_CONFIG: Record<AppSection, SectionConfig> = {
  home: {
    labelKey: "sidebar.home",
    icon: House,
    component: HomePage,
  },
  meeting: {
    labelKey: "sidebar.meeting",
    icon: Users,
    component: MeetingPage,
  },
  statistics: {
    labelKey: "sidebar.statistics",
    icon: ChartNoAxesCombined,
    component: StatisticsSettings,
  },
  settings: {
    labelKey: "sidebar.settings",
    icon: Settings2,
    component: SettingsPage,
  },
};

const ORDER: AppSection[] = ["home", "meeting", "statistics", "settings"];

interface SidebarProps {
  activeSection: SidebarSection;
  onSectionChange: (section: SidebarSection) => void;
}

export const Sidebar: React.FC<SidebarProps> = ({
  activeSection,
  onSectionChange,
}) => {
  const { t } = useTranslation();
  const [version, setVersion] = useState("");

  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => setVersion(""));
  }, []);

  return (
    <nav
      aria-label={t("shell.navigation")}
      className="pf-vibrant flex h-full w-46 shrink-0 flex-col px-2.5 pt-1.5 pb-3"
    >
      <ul className="flex flex-col gap-px">
        {ORDER.map((id) => {
          const section = SECTIONS_CONFIG[id];
          const Icon = section.icon;
          const isActive = activeSection === id;
          return (
            <li key={id} className="relative">
              {isActive && (
                <motion.span
                  layoutId="pf-nav-active"
                  className="absolute inset-0 rounded-[8px] bg-text/[0.075] shadow-[inset_0_1px_0_rgba(255,255,255,0.04)]"
                  transition={SPRING_SNAPPY}
                />
              )}
              <motion.button
                type="button"
                aria-current={isActive ? "page" : undefined}
                onClick={() => onSectionChange(id)}
                whileTap={{ scale: PRESS_SCALE }}
                transition={SPRING_SNAPPY}
                className={`relative flex h-8 w-full cursor-pointer items-center gap-2.5 rounded-[8px] px-2.5 text-[13px] font-medium tracking-[-0.006em] transition-colors duration-150 ${
                  isActive
                    ? "text-text"
                    : "text-text-muted hover:bg-text/[0.04] hover:text-text"
                }`}
              >
                <Icon
                  className={`h-4 w-4 shrink-0 transition-colors duration-150 ${
                    isActive ? "text-logo-primary" : ""
                  }`}
                  strokeWidth={isActive ? 2.1 : 1.8}
                />
                <span className="truncate" title={t(section.labelKey)}>
                  {t(section.labelKey)}
                </span>
              </motion.button>
            </li>
          );
        })}
      </ul>

      {version && (
        <p className="mt-auto px-2.5 text-[10.5px] tabular-nums text-text-muted/55">
          {t("shell.version", { version })}
        </p>
      )}
    </nav>
  );
};
