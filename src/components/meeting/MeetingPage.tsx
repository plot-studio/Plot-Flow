import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import {
  Loader2,
  Mic,
  MonitorSpeaker,
  PanelRightClose,
  PanelRightOpen,
  Pause,
  Play,
  Trash2,
  type LucideIcon,
} from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { PRESS_SCALE, SPRING, SPRING_SNAPPY } from "@/lib/motion";
import {
  commands,
  type MeetingDiarizerStatus,
  type MeetingInfo,
  type MeetingSources,
} from "@/bindings";
import { Dialog } from "@/components/ui/Dialog";
import { Button } from "@/components/ui/Button";
import { formatClock, isActiveState, useMeetingUi } from "./meetingStore";

const SOURCES_KEY = "plotflow.meeting.sources";

const loadSources = (): MeetingSources => {
  try {
    const raw = localStorage.getItem(SOURCES_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<MeetingSources>;
      return {
        microphone: parsed.microphone !== false,
        system_audio: parsed.system_audio !== false,
      };
    }
  } catch {
    // ignore
  }
  return { microphone: true, system_audio: true };
};

export const MeetingPage: React.FC = () => {
  const { t } = useTranslation();
  const status = useMeetingUi((s) => s.status);
  const refreshList = useMeetingUi((s) => s.refreshList);
  const active = isActiveState(status);

  useEffect(() => {
    void refreshList();
  }, [refreshList]);

  return (
    <div className="flex w-full max-w-[680px] flex-col gap-6">
      <header className="px-1">
        <h1 className="pf-title">{t("meeting.title")}</h1>
      </header>
      {status == null ? (
        <div className="flex items-center justify-center gap-2 py-16 text-sm text-text-muted">
          <Loader2 className="h-4 w-4 animate-spin" />
          {t("meeting.loadingStatus")}
        </div>
      ) : active ? (
        <ActiveRecorder />
      ) : (
        <IdleRecorder />
      )}
      <PastMeetings />
    </div>
  );
};

const IdleRecorder: React.FC = () => {
  const { t } = useTranslation();
  const [sources, setSources] = useState<MeetingSources>(loadSources);
  const [starting, setStarting] = useState(false);
  const set = useMeetingUi((s) => s.set);
  const openMeeting = useMeetingUi((s) => s.openMeeting);

  const toggle = (key: keyof MeetingSources) => {
    setSources((prev) => {
      const next = { ...prev, [key]: !prev[key] };
      try {
        localStorage.setItem(SOURCES_KEY, JSON.stringify(next));
      } catch {
        // ignore
      }
      return next;
    });
  };

  const none = !sources.microphone && !sources.system_audio;

  const start = async () => {
    if (none || starting) return;
    setStarting(true);
    try {
      const res = await commands.meetingStart(sources);
      if (res.status === "error") {
        toast.error(t("meeting.errors.start"), { description: res.error });
        return;
      }
      const info = res.data;
      set({
        detail: { info, speakers: [], segments: [] },
        summaryOpenFor: null,
        summaryError: null,
      });
      void openMeeting(info.id).catch(() => undefined);
      const st = await commands.meetingStatus();
      if (st.status === "ok") useMeetingUi.getState().setStatus(st.data);
    } finally {
      setStarting(false);
    }
  };

  const chip = (
    key: keyof MeetingSources,
    Icon: LucideIcon,
    label: string,
  ) => {
    const on = sources[key];
    return (
      <motion.button
        type="button"
        role="switch"
        aria-checked={on}
        aria-label={label}
        title={label}
        onClick={() => toggle(key)}
        whileTap={{ scale: 0.92 }}
        transition={SPRING_SNAPPY}
        className={`flex h-8 w-8 cursor-pointer items-center justify-center rounded-full transition-colors duration-150 ${
          on
            ? "bg-text/[0.1] text-text"
            : "text-text-muted/60 hover:bg-text/[0.05] hover:text-text-muted"
        }`}
      >
        <Icon className="h-4 w-4" strokeWidth={on ? 2 : 1.7} />
      </motion.button>
    );
  };

  return (
    <section className="pf-card flex flex-col items-center gap-5 px-6 pt-9 pb-6">
      <motion.button
        type="button"
        onClick={() => void start()}
        disabled={none || starting}
        aria-label={t("meeting.start")}
        title={t("meeting.start")}
        initial="rest"
        whileHover={none || starting ? undefined : "hover"}
        whileTap={none || starting ? undefined : "press"}
        variants={{
          rest: { scale: 1 },
          hover: { scale: 1 },
          press: { scale: PRESS_SCALE },
        }}
        transition={SPRING_SNAPPY}
        className="relative flex h-[72px] w-[72px] cursor-pointer items-center justify-center rounded-full border-[3px] border-text/[0.16] disabled:cursor-not-allowed disabled:opacity-40"
      >
        <motion.span
          variants={{
            rest: { scale: 1 },
            hover: { scale: 1.06 },
            press: { scale: 0.9 },
          }}
          transition={SPRING_SNAPPY}
          className="pf-accent flex h-[54px] w-[54px] items-center justify-center rounded-full"
        >
          {starting && <Loader2 className="h-5 w-5 animate-spin" />}
        </motion.span>
      </motion.button>
      <div className="flex items-center gap-1">
        {chip("microphone", Mic, t("meeting.sources.microphone"))}
        {chip("system_audio", MonitorSpeaker, t("meeting.sources.system"))}
      </div>
      {none && (
        <p className="-mt-2 text-[12px] text-warning">
          {t("meeting.sources.none")}
        </p>
      )}
      {sources.system_audio && <DiarizerOffer />}
    </section>
  );
};

/** One quiet line offering the speaker-separation model until it is installed. */
const DiarizerOffer: React.FC = () => {
  const { t } = useTranslation();
  const [status, setStatus] = useState<MeetingDiarizerStatus | null>(null);
  const [percent, setPercent] = useState<number | null>(null);
  const tRef = useRef(t);
  tRef.current = t;

  useEffect(() => {
    let alive = true;
    void commands
      .meetingDiarizerStatus()
      .then((res) => {
        if (!alive || res.status !== "ok") return;
        setStatus(res.data);
        if (res.data.installing) setPercent((p) => p ?? 0);
      })
      .catch(() => undefined);
    const unlisteners = [
      listen<{ downloaded: number; total: number }>(
        "meeting-diarizer-progress",
        (e) => {
          const { downloaded, total } = e.payload;
          setPercent(total > 0 ? Math.floor((downloaded / total) * 100) : 0);
        },
      ),
      listen<{ ok: boolean; error: string | null }>(
        "meeting-diarizer-finished",
        (e) => {
          setPercent(null);
          setStatus((s) =>
            s ? { ...s, installed: e.payload.ok, installing: false } : s,
          );
          if (e.payload.ok) {
            toast.success(tRef.current("meeting.diarizer.done"));
          } else {
            toast.error(tRef.current("meeting.diarizer.failed"), {
              description: e.payload.error ?? undefined,
            });
          }
        },
      ),
    ];
    return () => {
      alive = false;
      unlisteners.forEach((p) => void p.then((fn) => fn()));
    };
  }, []);

  if (!status || status.installed) return null;
  const downloading = percent != null;

  const install = async () => {
    if (downloading) return;
    setPercent(0);
    // The outcome arrives as `meeting-diarizer-finished`.
    const res = await commands.meetingDiarizerInstall().catch(() => null);
    if (res?.status !== "ok") {
      // Rejected before starting: show the offer again unless a download runs.
      const st = await commands.meetingDiarizerStatus().catch(() => null);
      if (!st || st.status !== "ok" || !st.data.installing) setPercent(null);
    }
  };

  return (
    <button
      type="button"
      onClick={() => void install()}
      disabled={downloading}
      className="-mt-1 cursor-pointer text-[12px] text-text-muted transition-colors duration-150 hover:text-text disabled:cursor-default disabled:hover:text-text-muted"
    >
      {downloading
        ? t("meeting.diarizer.downloading", { percent })
        : t("meeting.diarizer.offer", { size: status.size_mb })}
    </button>
  );
};

const Meter: React.FC<{ label: string; level: number }> = ({
  label,
  level,
}) => {
  // Levels arrive as 0..1 RMS; sqrt makes quiet speech visible.
  const pct = Math.round(Math.sqrt(Math.min(1, Math.max(0, level || 0))) * 100);
  return (
    <div className="flex items-center gap-3 text-[12px]">
      <span className="w-20 shrink-0 truncate text-end text-text-muted">
        {label}
      </span>
      <div className="h-[3px] flex-1 overflow-hidden rounded-full bg-text/[0.08]">
        <div
          className="h-full rounded-full bg-logo-primary transition-[width] duration-100 ease-out"
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
};

const ActiveRecorder: React.FC = () => {
  const { t } = useTranslation();
  const status = useMeetingUi((s) => s.status)!;
  const panelOpen = useMeetingUi((s) => s.panelOpen);
  const detailId = useMeetingUi((s) => s.detail?.info.id);
  const setPanel = useMeetingUi((s) => s.setPanel);
  const openMeeting = useMeetingUi((s) => s.openMeeting);
  const applyFinished = useMeetingUi((s) => s.applyFinished);
  const removeLocal = useMeetingUi((s) => s.removeMeetingLocal);
  const refreshList = useMeetingUi((s) => s.refreshList);
  const [busy, setBusy] = useState(false);
  const [confirmDiscard, setConfirmDiscard] = useState(false);

  const paused = status.state === "paused";
  const finalizing = status.state === "finalizing";
  const showingLive = panelOpen && detailId === status.meeting_id;

  const refreshStatus = async () => {
    const st = await commands.meetingStatus();
    if (st.status === "ok") useMeetingUi.getState().setStatus(st.data);
  };

  const run = async (
    fn: () => Promise<{ status: "ok" | "error"; error?: string }>,
    errKey: string,
  ) => {
    setBusy(true);
    try {
      const res = await fn();
      if (res.status === "error") {
        toast.error(t(errKey), { description: res.error });
      }
      await refreshStatus();
    } finally {
      setBusy(false);
    }
  };

  const stop = () =>
    run(async () => {
      const res = await commands.meetingStop();
      if (res.status === "ok") {
        applyFinished(res.data);
        void refreshList();
        return { status: "ok" as const };
      }
      return res;
    }, "meeting.errors.stop");

  const discard = async () => {
    setConfirmDiscard(false);
    const id = status.meeting_id;
    await run(async () => {
      const res = await commands.meetingDiscard();
      if (res.status === "ok") {
        if (id != null) removeLocal(id);
        toast(t("meeting.toast.discarded"));
        void refreshList();
      }
      return res;
    }, "meeting.errors.discard");
  };

  const togglePanel = () => {
    if (showingLive) void setPanel(false);
    else if (status.meeting_id != null) {
      void openMeeting(status.meeting_id).catch(() => undefined);
    }
  };

  return (
    <section className="pf-card flex flex-col items-center gap-5 px-6 pt-6 pb-5">
      <div className="flex items-center gap-2 text-[12px] font-medium">
        <span
          className={`h-1.5 w-1.5 rounded-full ${
            finalizing
              ? "bg-mid-gray"
              : paused
                ? "bg-warning"
                : "animate-pulse bg-[#e5534b]"
          }`}
        />
        <span className="text-text-muted">
          {t(`meeting.state.${status.state}`)}
        </span>
      </div>

      <div className="text-[44px] leading-none font-medium tracking-[-0.035em] tabular-nums">
        {formatClock(status.elapsed_ms)}
      </div>

      <motion.button
        type="button"
        onClick={() => void stop()}
        disabled={busy || finalizing}
        aria-label={t("meeting.stop")}
        title={t("meeting.stop")}
        whileTap={busy || finalizing ? undefined : { scale: PRESS_SCALE }}
        transition={SPRING_SNAPPY}
        className="flex h-[72px] w-[72px] cursor-pointer items-center justify-center rounded-full border-[3px] border-text/[0.16] disabled:cursor-not-allowed disabled:opacity-50"
      >
        {finalizing ? (
          <Loader2 className="h-6 w-6 animate-spin text-text-muted" />
        ) : (
          <span className="h-7 w-7 rounded-[7px] bg-[#e5534b] shadow-[inset_0_1px_0_rgba(255,255,255,0.2)]" />
        )}
      </motion.button>

      {finalizing ? (
        <p className="text-xs text-text-muted">
          {t("meeting.finalizing.hint")}
        </p>
      ) : (
        <div className="flex w-full max-w-sm flex-col gap-2">
          <Meter label={t("meeting.levels.me")} level={status.mic_level} />
          <Meter
            label={t("meeting.levels.others")}
            level={status.system_level}
          />
        </div>
      )}

      <div className="flex flex-wrap items-center justify-center gap-2">
        <Button
          variant="secondary"
          size="sm"
          disabled={busy || finalizing}
          onClick={() =>
            void run(
              paused ? commands.meetingResume : commands.meetingPause,
              paused ? "meeting.errors.resume" : "meeting.errors.pause",
            )
          }
          className="inline-flex items-center gap-1.5"
        >
          {paused ? (
            <Play className="h-3.5 w-3.5" />
          ) : (
            <Pause className="h-3.5 w-3.5" />
          )}
          {paused ? t("meeting.resume") : t("meeting.pause")}
        </Button>
        <Button
          variant="secondary"
          size="sm"
          onClick={togglePanel}
          className="inline-flex items-center gap-1.5"
        >
          {showingLive ? (
            <PanelRightClose className="h-3.5 w-3.5" />
          ) : (
            <PanelRightOpen className="h-3.5 w-3.5" />
          )}
          {showingLive ? t("meeting.panel.hide") : t("meeting.panel.show")}
        </Button>
        <Button
          variant="danger-ghost"
          size="sm"
          disabled={busy || finalizing}
          onClick={() => setConfirmDiscard(true)}
          className="inline-flex items-center gap-1.5"
        >
          <Trash2 className="h-3.5 w-3.5" />
          {t("meeting.discard")}
        </Button>
      </div>

      {status.last_error && (
        <p className="text-center text-xs text-error">{status.last_error}</p>
      )}

      <Dialog
        open={confirmDiscard}
        onOpenChange={setConfirmDiscard}
        title={t("meeting.dialog.discardTitle")}
        closeLabel={t("meeting.dialog.close")}
        footer={
          <>
            <Button
              variant="secondary"
              size="sm"
              onClick={() => setConfirmDiscard(false)}
            >
              {t("meeting.dialog.cancel")}
            </Button>
            <Button variant="danger" size="sm" onClick={() => void discard()}>
              {t("meeting.dialog.discardConfirm")}
            </Button>
          </>
        }
      >
        <p className="text-sm text-text-muted">
          {t("meeting.dialog.discardBody")}
        </p>
      </Dialog>
    </section>
  );
};

const PastMeetings: React.FC = () => {
  const { t, i18n } = useTranslation();
  const meetings = useMeetingUi((s) => s.meetings);
  const listError = useMeetingUi((s) => s.listError);
  const refreshList = useMeetingUi((s) => s.refreshList);
  const openMeeting = useMeetingUi((s) => s.openMeeting);
  const removeLocal = useMeetingUi((s) => s.removeMeetingLocal);
  const status = useMeetingUi((s) => s.status);
  const viewedId = useMeetingUi((s) =>
    s.panelOpen ? s.detail?.info.id : null,
  );
  const [toDelete, setToDelete] = useState<MeetingInfo | null>(null);

  const liveId = isActiveState(status) ? status?.meeting_id : null;
  const past = (meetings ?? []).filter((m) => m.id !== liveId);

  const formatDate = (ms: number) => {
    const d = new Date(ms);
    const now = new Date();
    const dayDiff = Math.round(
      (new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() -
        new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()) /
        86_400_000,
    );
    const time = d.toLocaleTimeString(i18n.language, {
      hour: "2-digit",
      minute: "2-digit",
    });
    if (dayDiff === 0) return `${t("meeting.date.today")}, ${time}`;
    if (dayDiff === 1) return `${t("meeting.date.yesterday")}, ${time}`;
    return d.toLocaleString(i18n.language, {
      day: "numeric",
      month: "short",
      year: d.getFullYear() === now.getFullYear() ? undefined : "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  };

  const open = (id: number) => {
    void openMeeting(id).catch((e) =>
      toast.error(t("meeting.errors.detail"), { description: String(e) }),
    );
  };

  const doDelete = async () => {
    const m = toDelete;
    setToDelete(null);
    if (!m) return;
    const res = await commands.meetingDelete(m.id);
    if (res.status === "error") {
      toast.error(t("meeting.errors.delete"), { description: res.error });
      return;
    }
    removeLocal(m.id);
    toast(t("meeting.toast.deleted"));
  };

  return (
    <section className="flex flex-col gap-2">
      <h2 className="pf-label px-1">{t("meeting.list.title")}</h2>
      {meetings == null && !listError ? (
        <p className="px-1 text-sm text-text-muted">
          {t("meeting.list.loading")}
        </p>
      ) : listError && meetings == null ? (
        <div className="flex items-center gap-2 px-1 text-sm text-error">
          {t("meeting.errors.list")}
          <button
            type="button"
            onClick={() => void refreshList()}
            className="cursor-pointer underline"
          >
            {t("meeting.list.retry")}
          </button>
        </div>
      ) : past.length === 0 ? (
        <p className="px-1 py-2 text-[13px] text-text-muted">
          {t("meeting.list.empty")}
        </p>
      ) : (
        <ul className="pf-card pf-rows flex flex-col overflow-hidden">
          <AnimatePresence initial={false}>
            {past.map((m) => (
              <motion.li
                key={m.id}
                layout="position"
                initial={{ opacity: 0, height: 0 }}
                animate={{ opacity: 1, height: "auto" }}
                exit={{ opacity: 0, height: 0 }}
                transition={SPRING}
                className="overflow-hidden"
              >
                <div
                  className={`group flex items-center gap-3 px-4 py-3 transition-colors duration-150 ${
                    viewedId === m.id
                      ? "bg-text/[0.05]"
                      : "hover:bg-text/[0.018]"
                  }`}
                >
                  <button
                    type="button"
                    onClick={() => open(m.id)}
                    className="min-w-0 flex-1 cursor-pointer text-start"
                  >
                    <div className="flex items-center gap-2">
                      <span className="truncate text-[14px] tracking-[-0.006em]">
                        {m.title || t("meeting.untitled")}
                      </span>
                      {m.status === "interrupted" && (
                        <span className="shrink-0 rounded-full bg-warning/12 px-1.5 py-px text-[10.5px] font-medium text-warning">
                          {t("meeting.list.status.interrupted")}
                        </span>
                      )}
                      {m.status === "recording" && (
                        <span className="shrink-0 rounded-full bg-[#e5534b]/12 px-1.5 py-px text-[10.5px] font-medium text-[#e5534b]">
                          {t("meeting.list.status.recording")}
                        </span>
                      )}
                    </div>
                    <div className="mt-1 flex gap-2 text-[12px] text-text-muted">
                      <span>{formatDate(m.started_at)}</span>
                      <span>·</span>
                      <span className="tabular-nums">
                        {formatClock(m.duration_ms)}
                      </span>
                      {m.speaker_count > 0 && (
                        <>
                          <span>·</span>
                          <span>
                            {t("meeting.list.speakers", {
                              count: m.speaker_count,
                            })}
                          </span>
                        </>
                      )}
                    </div>
                  </button>
                  <button
                    type="button"
                    onClick={() => setToDelete(m)}
                    aria-label={t("meeting.list.delete")}
                    title={t("meeting.list.delete")}
                    className="flex h-7 w-7 shrink-0 cursor-pointer items-center justify-center rounded-md text-text-muted opacity-0 transition-[opacity,color,background-color] duration-150 group-hover:opacity-100 hover:bg-text/[0.07] hover:text-text focus-visible:opacity-100 active:scale-[0.92]"
                  >
                    <Trash2 className="h-[15px] w-[15px]" />
                  </button>
                </div>
              </motion.li>
            ))}
          </AnimatePresence>
        </ul>
      )}

      <Dialog
        open={toDelete != null}
        onOpenChange={(o) => !o && setToDelete(null)}
        title={t("meeting.dialog.deleteTitle")}
        closeLabel={t("meeting.dialog.close")}
        footer={
          <>
            <Button
              variant="secondary"
              size="sm"
              onClick={() => setToDelete(null)}
            >
              {t("meeting.dialog.cancel")}
            </Button>
            <Button variant="danger" size="sm" onClick={() => void doDelete()}>
              {t("meeting.dialog.deleteConfirm")}
            </Button>
          </>
        }
      >
        <p className="text-sm text-text-muted">
          {t("meeting.dialog.deleteBody", {
            title: toDelete?.title || t("meeting.untitled"),
          })}
        </p>
      </Dialog>
    </section>
  );
};
