import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  Loader2,
  Mic,
  MonitorSpeaker,
  PanelRightClose,
  PanelRightOpen,
  Pause,
  Play,
  Square,
  Trash2,
} from "lucide-react";
import { commands, type MeetingInfo, type MeetingSources } from "@/bindings";
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
    <div className="flex w-full max-w-2xl flex-col gap-5">
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
    Icon: React.ComponentType<{ className?: string }>,
    label: string,
  ) => {
    const on = sources[key];
    return (
      <button
        type="button"
        role="switch"
        aria-checked={on}
        onClick={() => toggle(key)}
        className={`inline-flex cursor-pointer items-center gap-2 rounded-full border px-3.5 py-1.5 text-sm font-medium transition-colors ${
          on
            ? "border-logo-primary/70 bg-logo-primary/15 text-text"
            : "border-line bg-transparent text-text-muted hover:border-mid-gray/50"
        }`}
      >
        <Icon className={`h-4 w-4 ${on ? "text-logo-primary" : ""}`} />
        {label}
      </button>
    );
  };

  return (
    <section className="flex flex-col items-center gap-4 rounded-xl border border-line bg-surface px-6 pt-6 pb-5 text-center">
      <div>
        <h1 className="text-lg font-semibold">{t("meeting.title")}</h1>
        <p className="mt-0.5 text-sm text-text-muted">
          {t("meeting.subtitle")}
        </p>
      </div>
      <button
        type="button"
        onClick={() => void start()}
        disabled={none || starting}
        aria-label={t("meeting.start")}
        title={t("meeting.start")}
        className="group relative flex h-24 w-24 cursor-pointer items-center justify-center rounded-full bg-logo-primary text-on-accent shadow-[0_0_0_6px_rgba(150,163,108,0.15)] transition-transform hover:scale-105 active:scale-95 disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:scale-100"
      >
        {starting ? (
          <Loader2 className="h-9 w-9 animate-spin" />
        ) : (
          <Mic className="h-10 w-10" strokeWidth={2.2} />
        )}
      </button>
      <span className="-mt-1 text-xs font-medium text-text-muted">
        {t("meeting.start")}
      </span>
      <div className="flex flex-wrap justify-center gap-2">
        {chip("microphone", Mic, t("meeting.sources.microphone"))}
        {chip("system_audio", MonitorSpeaker, t("meeting.sources.system"))}
      </div>
      <p className={`text-xs ${none ? "text-warning" : "text-text-muted"}`}>
        {none ? t("meeting.sources.none") : t("meeting.sources.hint")}
      </p>
    </section>
  );
};

const Meter: React.FC<{ label: string; level: number }> = ({
  label,
  level,
}) => {
  // Levels arrive as 0..1 RMS; sqrt makes quiet speech visible.
  const pct = Math.round(Math.sqrt(Math.min(1, Math.max(0, level || 0))) * 100);
  return (
    <div className="flex items-center gap-2 text-xs">
      <span className="w-20 shrink-0 truncate text-end text-text-muted">
        {label}
      </span>
      <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-line">
        <div
          className="h-full rounded-full bg-logo-primary transition-[width] duration-150"
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
    <section className="flex flex-col items-center gap-4 rounded-xl border border-line bg-surface px-6 pt-6 pb-5">
      <div className="flex items-center gap-2 text-xs font-medium">
        <span
          className={`h-2 w-2 rounded-full ${
            finalizing
              ? "bg-mid-gray"
              : paused
                ? "bg-warning"
                : "animate-pulse bg-red-500"
          }`}
        />
        <span className="text-text-muted">
          {t(`meeting.state.${status.state}`)}
        </span>
      </div>

      <div className="font-mono text-5xl font-light tracking-tight text-text tabular-nums">
        {formatClock(status.elapsed_ms)}
      </div>

      <button
        type="button"
        onClick={() => void stop()}
        disabled={busy || finalizing}
        aria-label={t("meeting.stop")}
        title={t("meeting.stop")}
        className="flex h-20 w-20 cursor-pointer items-center justify-center rounded-full bg-red-600 text-white shadow-[0_0_0_6px_rgba(220,38,38,0.15)] transition-transform hover:scale-105 hover:bg-red-500 active:scale-95 disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:scale-100"
      >
        {finalizing ? (
          <Loader2 className="h-8 w-8 animate-spin" />
        ) : (
          <Square className="h-7 w-7" fill="currentColor" />
        )}
      </button>

      {finalizing ? (
        <p className="text-xs text-text-muted">
          {t("meeting.finalizing.hint")}
        </p>
      ) : (
        <div className="flex w-full max-w-sm flex-col gap-1.5">
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
      <h2 className="px-1 text-xs font-semibold tracking-wide text-text-muted uppercase">
        {t("meeting.list.title")}
      </h2>
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
        <p className="rounded-lg border border-dashed border-line px-4 py-6 text-center text-sm text-text-muted">
          {t("meeting.list.empty")}
        </p>
      ) : (
        <ul className="flex flex-col overflow-hidden rounded-xl border border-line bg-surface">
          {past.map((m) => (
            <li
              key={m.id}
              className={`group flex items-center gap-3 border-b border-line px-3 py-2.5 last:border-b-0 ${
                viewedId === m.id ? "bg-logo-primary/10" : "hover:bg-surface-2"
              }`}
            >
              <button
                type="button"
                onClick={() => open(m.id)}
                className="min-w-0 flex-1 cursor-pointer text-start"
              >
                <div className="flex items-center gap-2">
                  <span className="truncate text-sm font-medium">
                    {m.title || t("meeting.untitled")}
                  </span>
                  {m.status === "interrupted" && (
                    <span className="shrink-0 rounded-full bg-warning/15 px-1.5 py-px text-[10px] font-semibold text-warning">
                      {t("meeting.list.status.interrupted")}
                    </span>
                  )}
                  {m.status === "recording" && (
                    <span className="shrink-0 rounded-full bg-red-500/15 px-1.5 py-px text-[10px] font-semibold text-red-400">
                      {t("meeting.list.status.recording")}
                    </span>
                  )}
                </div>
                <div className="mt-0.5 flex gap-2 text-xs text-text-muted">
                  <span>{formatDate(m.started_at)}</span>
                  <span>·</span>
                  <span className="tabular-nums">
                    {formatClock(m.duration_ms)}
                  </span>
                  {m.speaker_count > 0 && (
                    <>
                      <span>·</span>
                      <span>
                        {t("meeting.list.speakers", { count: m.speaker_count })}
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
                className="shrink-0 cursor-pointer rounded-md p-1.5 text-text-muted opacity-0 transition-opacity group-hover:opacity-100 hover:bg-red-500/10 hover:text-red-400 focus-visible:opacity-100"
              >
                <Trash2 className="h-4 w-4" />
              </button>
            </li>
          ))}
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
