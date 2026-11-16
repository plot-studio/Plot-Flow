import React, { useEffect, useRef } from "react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import {
  commands,
  type MeetingInfo,
  type MeetingSegment,
  type MeetingStatus,
} from "@/bindings";
import {
  MEETING_PANEL_WIDTH,
  isActiveState,
  resizeWindow,
  useMeetingUi,
  type LlmInstallProgress,
} from "./meetingStore";
import { MeetingPanel } from "./MeetingPanel";
import { SPRING } from "@/lib/motion";

/**
 * Always-mounted host for meeting mode: subscribes to backend events once,
 * recovers a running meeting on startup and renders the right-hand panel.
 */
export const MeetingPanelSlot: React.FC = () => {
  const { t } = useTranslation();
  const panelOpen = useMeetingUi((s) => s.panelOpen);
  const reduceMotion = useReducedMotion();
  const lastLiveId = useRef<number | null>(null);
  const speakerFetchTimer = useRef<number | null>(null);
  const tRef = useRef(t);
  tRef.current = t;

  useEffect(() => {
    const store = useMeetingUi.getState;

    const followStatus = (status: MeetingStatus) => {
      store().setStatus(status);
      const active = isActiveState(status);
      if (active && status.meeting_id != null) {
        if (lastLiveId.current !== status.meeting_id) {
          lastLiveId.current = status.meeting_id;
          // A meeting started elsewhere (or recovered): show it live.
          if (store().detail?.info.id !== status.meeting_id) {
            void store()
              .openMeeting(status.meeting_id)
              .catch((e) => console.warn("meeting_get failed:", e));
          } else {
            void store().setPanel(true);
          }
        }
      } else if (!active) {
        lastLiveId.current = null;
      }
    };

    // The native window keeps its size across a reload/crash of the webview:
    // without a live meeting (and no panel shown) put it back to normal width.
    const shrinkIfIdle = (status: MeetingStatus | null) => {
      if ((!status || !isActiveState(status)) && !store().panelOpen) {
        void resizeWindow(false);
      }
    };

    // Recover state on mount.
    void commands
      .meetingStatus()
      .then((res) => {
        if (res.status === "ok") {
          followStatus(res.data);
          shrinkIfIdle(res.data);
        } else {
          shrinkIfIdle(null);
        }
      })
      .catch((e) => {
        console.warn("meeting_status failed:", e);
        shrinkIfIdle(null);
      });
    void store().refreshLlm();

    const scheduleSpeakerFetch = () => {
      if (speakerFetchTimer.current != null) return;
      speakerFetchTimer.current = window.setTimeout(() => {
        speakerFetchTimer.current = null;
        void store().reloadDetail();
      }, 800);
    };

    const unlisteners = [
      listen<MeetingStatus>("meeting-status", (e) => followStatus(e.payload)),
      listen<MeetingSegment>("meeting-segment", (e) => {
        const seg = e.payload;
        store().upsertSegment(seg);
        const detail = store().detail;
        if (
          seg.speaker_id != null &&
          detail?.info.id === seg.meeting_id &&
          !detail.speakers.some((s) => s.id === seg.speaker_id)
        ) {
          scheduleSpeakerFetch();
        }
      }),
      listen<MeetingInfo>("meeting-finished", (e) => {
        store().applyFinished(e.payload);
        void store().refreshList();
        void store().reloadDetail();
      }),
      listen<{ message: string }>("meeting-error", (e) => {
        toast.error(tRef.current("meeting.toast.error"), {
          description: e.payload?.message,
        });
      }),
      listen<LlmInstallProgress>("llm-install-progress", (e) => {
        store().set({ llmProgress: e.payload, llmInstalling: true });
      }),
      listen<{ ok: boolean; error: string | null }>(
        "llm-install-finished",
        (e) => {
          store().set({ llmInstalling: false, llmProgress: null });
          if (e.payload.ok) {
            toast.success(tRef.current("meeting.summary.installDone"));
            void store()
              .refreshLlm()
              .then((llm) => {
                const pending = store().pendingSummaryId;
                if (llm?.installed && pending != null) {
                  void store().summarize(pending);
                }
              });
          } else {
            toast.error(tRef.current("meeting.summary.installFailed"), {
              description: e.payload.error ?? undefined,
            });
          }
        },
      ),
    ];

    return () => {
      if (speakerFetchTimer.current != null) {
        window.clearTimeout(speakerFetchTimer.current);
      }
      unlisteners.forEach((p) => void p.then((fn) => fn()));
    };
  }, []);

  return (
    <AnimatePresence initial={false}>
      {panelOpen && (
        <motion.aside
          key="meeting-panel"
          className="h-full shrink-0 overflow-hidden border-s border-text/[0.07] bg-surface"
          initial={reduceMotion ? { opacity: 0 } : { width: 0, opacity: 0 }}
          animate={{ width: MEETING_PANEL_WIDTH, opacity: 1 }}
          exit={reduceMotion ? { opacity: 0 } : { width: 0, opacity: 0 }}
          transition={SPRING}
        >
          <div
            className="h-full"
            style={{
              width: MEETING_PANEL_WIDTH,
              minWidth: MEETING_PANEL_WIDTH,
            }}
          >
            <MeetingPanel />
          </div>
        </motion.aside>
      )}
    </AnimatePresence>
  );
};
