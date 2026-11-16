import {
  useEffect,
  useLayoutEffect,
  useState,
  useRef,
  useCallback,
  type ReactNode,
} from "react";
import { toast, Toaster } from "sonner";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { platform } from "@tauri-apps/plugin-os";
import {
  checkAccessibilityPermission,
  checkMicrophonePermission,
} from "tauri-plugin-macos-permissions-api";
import { ModelStateEvent, RecordingErrorEvent } from "./lib/types/events";
import "./App.css";
import { AnimatePresence, MotionConfig, motion } from "motion/react";
import AccessibilityPermissions from "./components/AccessibilityPermissions";
import SecureInputWarning from "./components/SecureInputWarning";
import AccessibilityOnboarding from "./components/onboarding/AccessibilityOnboarding";
import { BundledModelStatus } from "./components/BundledModelStatus";
import { useBundledModel } from "./hooks/useBundledModel";
import { withTimeout } from "./lib/bundledModel";
import { Sidebar, SidebarSection, SECTIONS_CONFIG } from "./components/Sidebar";
import { TitleBar } from "./components/shell/TitleBar";
import { MeetingPanelSlot } from "./components/meeting";
import { useSettingsStore } from "./stores/settingsStore";
import { useNavigation } from "./stores/navigationStore";
import { commands } from "@/bindings";
import { SPRING, SPRING_PAGE } from "@/lib/motion";
import { getLanguageDirection, initializeRTL } from "@/lib/utils/rtl";

type OnboardingStep = "accessibility" | "done";

const renderSettingsContent = (section: SidebarSection) => {
  const ActiveComponent = SECTIONS_CONFIG[section].component;
  return <ActiveComponent />;
};

function App() {
  const { t, i18n } = useTranslation();
  const [onboardingStep, setOnboardingStep] = useState<OnboardingStep | null>(
    null,
  );
  const [startupError, setStartupError] = useState(false);
  const { status: modelStatus, refresh: refreshModel } = useBundledModel();
  const currentSection = useNavigation((s) => s.section);
  const setCurrentSection = useNavigation((s) => s.setSection);
  const scrollRef = useRef<HTMLDivElement>(null);
  const direction = getLanguageDirection(i18n.language);
  const refreshAudioDevices = useSettingsStore(
    (state) => state.refreshAudioDevices,
  );
  const refreshOutputDevices = useSettingsStore(
    (state) => state.refreshOutputDevices,
  );
  const hasCompletedPostOnboardingInit = useRef(false);
  const isShowingOnboarding = onboardingStep === "accessibility";

  // Classic scrollbars consume layout space. Reserve a matching gutter on the
  // opposite edge while onboarding is visible so its content stays centered in
  // the physical window. Overlay scrollbars ignore scrollbar-gutter.
  useLayoutEffect(() => {
    const attribute = "data-onboarding-active";
    document.documentElement.toggleAttribute(attribute, isShowingOnboarding);
    return () => document.documentElement.removeAttribute(attribute);
  }, [isShowingOnboarding]);

  // Initialize RTL direction when language changes
  useEffect(() => {
    initializeRTL(i18n.language);
  }, [i18n.language]);

  // Initialize Enigo, shortcuts, and refresh audio devices when main app loads
  useEffect(() => {
    if (onboardingStep === "done" && !hasCompletedPostOnboardingInit.current) {
      hasCompletedPostOnboardingInit.current = true;
      Promise.all([
        commands.initializeEnigo(),
        commands.initializeShortcuts(),
      ]).catch((e) => {
        console.warn("Failed to initialize:", e);
      });
      refreshAudioDevices();
      refreshOutputDevices();
    }
  }, [onboardingStep, refreshAudioDevices, refreshOutputDevices]);

  // Listen for recording errors from the backend and show a toast
  useEffect(() => {
    const unlisten = listen<RecordingErrorEvent>("recording-error", (event) => {
      const { error_type, detail } = event.payload;

      if (error_type === "microphone_permission_denied") {
        const currentPlatform = platform();
        const platformKey = `errors.micPermissionDenied.${currentPlatform}`;
        const description = t(platformKey, {
          defaultValue: t("errors.micPermissionDenied.generic"),
        });
        toast.error(t("errors.micPermissionDeniedTitle"), { description });
      } else if (error_type === "no_input_device") {
        toast.error(t("errors.noInputDeviceTitle"), {
          description: t("errors.noInputDevice"),
        });
      } else {
        toast.error(
          t("errors.recordingFailed", { error: detail ?? "Unknown error" }),
        );
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  // Listen for paste failures and show a toast.
  // The technical error detail is logged to handy.log on the Rust side
  // (see actions.rs `error!("Failed to paste transcription: ...")`),
  // so we show a localized, user-friendly message here instead of the raw error.
  useEffect(() => {
    const unlisten = listen("paste-error", () => {
      toast.error(t("errors.pasteFailedTitle"), {
        description: t("errors.pasteFailed"),
      });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  // Listen for transcription failures and show a toast.
  // The payload is the backend error message (also logged to handy.log).
  useEffect(() => {
    const unlisten = listen<string>("transcription-error", (event) => {
      toast.error(t("errors.transcriptionFailedTitle"), {
        description: event.payload,
      });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  // Listen for model loading failures and show a toast
  useEffect(() => {
    const unlisten = listen<ModelStateEvent>("model-state-changed", (event) => {
      if (event.payload.event_type === "loading_failed") {
        toast.error(
          t("errors.modelLoadFailed", {
            model:
              event.payload.model_name || t("errors.modelLoadFailedUnknown"),
          }),
          {
            description: event.payload.error,
          },
        );
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  const revealMainWindowForPermissions = async () => {
    try {
      await withTimeout(commands.showMainWindowCommand());
    } catch (e) {
      console.warn("Failed to show main window for permission onboarding:", e);
    }
  };

  const checkOnboardingStatus = useCallback(async () => {
    setStartupError(false);
    setOnboardingStep(null);
    try {
      const settingsResult = await withTimeout(commands.getAppSettings());
      if (settingsResult.status === "error")
        throw new Error(settingsResult.error);
      const hasCompletedOnboarding =
        settingsResult.status === "ok" &&
        settingsResult.data.onboarding_completed === true;
      const currentPlatform = platform();

      if (hasCompletedOnboarding) {
        // Returning user - check if they need to grant permissions first

        if (currentPlatform === "macos") {
          try {
            const [hasAccessibility, hasMicrophone] = await withTimeout(
              Promise.all([
                checkAccessibilityPermission(),
                checkMicrophonePermission(),
              ]),
            );
            if (!hasAccessibility || !hasMicrophone) {
              await revealMainWindowForPermissions();
              setOnboardingStep("accessibility");
              return;
            }
          } catch (e) {
            console.warn("Failed to check macOS permissions:", e);
            // If we can't check, proceed to main app and let them fix it there
          }
        }

        if (currentPlatform === "windows") {
          try {
            const microphoneStatus = await withTimeout(
              commands.getWindowsMicrophonePermissionStatus(),
            );
            if (
              microphoneStatus.supported &&
              microphoneStatus.overall_access === "denied"
            ) {
              await revealMainWindowForPermissions();
              setOnboardingStep("accessibility");
              return;
            }
          } catch (e) {
            console.warn("Failed to check Windows microphone permissions:", e);
            // If we can't check, proceed to main app and let them fix it there
          }
        }

        setOnboardingStep("done");
      } else {
        // New user - start full onboarding
        setOnboardingStep("accessibility");
      }
    } catch (error) {
      console.error("Failed to check onboarding status:", error);
      setStartupError(true);
    }
  }, []);

  useEffect(() => {
    void checkOnboardingStatus();
  }, [checkOnboardingStatus]);

  const handleAccessibilityComplete = useCallback(() => {
    // The bundled model is selected by the backend; there is no model wizard.
    setOnboardingStep("done");
  }, []);

  // Keep the Toaster mounted across permissions and the main interface so
  // errors remain visible when onboarding finishes.
  const toaster = (
    <Toaster
      theme="system"
      toastOptions={{
        unstyled: true,
        classNames: {
          toast:
            "bg-surface-2/85 backdrop-blur-xl backdrop-saturate-150 text-text border border-text/8 rounded-xl shadow-[inset_0_1px_0_rgba(255,255,255,0.04),0_16px_40px_-12px_rgba(0,0,0,0.6)] px-4 py-3 flex items-center gap-3 text-[13px] w-[340px] font-sans",
          title: "font-medium tracking-tight",
          description: "text-text-muted text-xs break-all",
          actionButton:
            "px-3 py-1 text-xs font-medium rounded-md border bg-surface border-text/10 hover:bg-text/5 cursor-pointer whitespace-nowrap",
        },
      }}
    />
  );

  // Frameless window: every state renders inside the custom title bar shell.
  const shell = (body: ReactNode) => (
    <MotionConfig reducedMotion="user" transition={SPRING}>
      {toaster}
      <div className="pf-material h-screen flex flex-col overflow-hidden text-text">
        <TitleBar />
        <div className="relative flex-1 min-h-0">{body}</div>
      </div>
    </MotionConfig>
  );

  // Still checking onboarding status
  if (onboardingStep === null) {
    return shell(
      <>
        <div className="h-full flex items-center justify-center p-4">
          <div
            role={startupError ? "alert" : "status"}
            className="text-sm text-center"
          >
            <p>
              {t(startupError ? "plotFlow.startupError" : "plotFlow.startup")}
            </p>
            {startupError && (
              <button
                type="button"
                onClick={() => void checkOnboardingStatus()}
                className="mt-2 underline cursor-pointer"
              >
                {t("plotFlow.retry")}
              </button>
            )}
          </div>
        </div>
      </>,
    );
  }

  // Select the content for the current step. The Toaster is rendered once, in a
  // stable wrapper around this node, so crossing between onboarding steps and
  // the main app never remounts it (which would drop any in-flight toast).
  let content: ReactNode;
  if (onboardingStep === "accessibility") {
    content = (
      <AccessibilityOnboarding onComplete={handleAccessibilityComplete} />
    );
  } else {
    content = (
      <div
        dir={direction}
        className="h-full flex select-none cursor-default overflow-hidden"
      >
        <Sidebar
          activeSection={currentSection}
          onSectionChange={setCurrentSection}
        />
        {/* Inset content sheet: page column + optional meeting panel */}
        <div className="pf-sheet mb-2 me-2 flex min-w-0 flex-1 overflow-hidden rounded-[12px]">
          <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
            <div
              ref={scrollRef}
              className="flex-1 overflow-y-auto overflow-x-hidden"
            >
              <div className="flex flex-col items-center gap-4 px-8 pt-7 pb-10">
                <AccessibilityPermissions />
                <SecureInputWarning />
                <BundledModelStatus
                  status={modelStatus}
                  onRetry={() => void refreshModel()}
                />
                <AnimatePresence
                  mode="wait"
                  initial={false}
                  onExitComplete={() => scrollRef.current?.scrollTo({ top: 0 })}
                >
                  <motion.div
                    key={currentSection}
                    className="flex w-full flex-col items-center"
                    initial={{ opacity: 0, y: 6 }}
                    animate={{ opacity: 1, y: 0 }}
                    exit={{
                      opacity: 0,
                      transition: { duration: 0.08, ease: "easeOut" },
                    }}
                    transition={SPRING_PAGE}
                  >
                    {renderSettingsContent(currentSection)}
                  </motion.div>
                </AnimatePresence>
              </div>
            </div>
          </div>
          <MeetingPanelSlot />
        </div>
      </div>
    );
  }

  return shell(content);
}

export default App;
