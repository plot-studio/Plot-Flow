import { create } from "zustand";

export type AppSection = "home" | "meeting" | "statistics" | "settings";

interface NavigationState {
  section: AppSection;
  setSection: (section: AppSection) => void;
}

/** Which main-window section is shown. Home is the default landing page. */
export const useNavigation = create<NavigationState>((set) => ({
  section: "home",
  setSection: (section) => set({ section }),
}));
