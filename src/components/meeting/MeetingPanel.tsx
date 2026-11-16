import React, { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import ReactMarkdown from "react-markdown";
import { toast } from "sonner";
import {
  ArrowDown,
  Bot,
  ChevronDown,
  Download,
  Loader2,
  RefreshCw,
  X,
} from "lucide-react";
import {
  commands,
  type MeetingExportFormat,
  type MeetingSegment,
  type MeetingSpeaker,
} from "@/bindings";
import {
  findSpeaker,
  formatClock,
  isActiveState,
  speakerColor,
  useMeetingUi,
} from "./meetingStore";

const EXPORT_FORMATS: MeetingExportFormat[] = ["txt", "md", "json", "srt"];

const iconBtn =
  "inline-flex h-7 items-center gap-1.5 rounded-lg border border-text/[0.06] bg-text/[0.06] px-2.5 text-xs font-medium text-text transition-[background-color,border-color,transform] duration-100 hover:bg-text/[0.1] enabled:active:scale-[0.97] disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer";

export const MeetingPanel: React.FC = () => {
  const { t } = useTranslation();
  const detail = useMeetingUi((s) => s.detail);
  const detailLoading = useMeetingUi((s) => s.detailLoading);
  const status = useMeetingUi((s) => s.status);
  const setPanel = useMeetingUi((s) => s.setPanel);

  const isLive =
    !!detail && isActiveState(status) && status?.meeting_id === detail.info.id;

  return (
    <div className="flex h-full flex-col text-text">
      <div className="flex h-12 items-center gap-2 border-b border-line px-3">
        {detail ? (
          <TitleEditor meetingId={detail.info.id} title={detail.info.title} />
        ) : (
          <div className="flex-1" />
        )}
        {isLive && (
          <span className="flex shrink-0 items-center gap-1.5 rounded-full bg-red-500/15 px-2 py-0.5 text-[11px] font-medium text-red-400">
            <span
              className={`h-1.5 w-1.5 rounded-full bg-red-500 ${
                status?.state === "recording" ? "animate-pulse" : ""
              }`}
            />
            {status?.state === "paused"
              ? t("meeting.state.paused")
              : t("meeting.panel.live")}
          </span>
        )}
        <button
          type="button"
          onClick={() => void setPanel(false)}
          aria-label={t("meeting.panel.close")}
          title={t("meeting.panel.close")}
          className="shrink-0 cursor-pointer rounded-full p-1.5 text-text-muted transition-colors hover:bg-text/6 hover:text-text"
        >
          <X className="h-4 w-4" />
        </button>
      </div>

      {detail ? (
        <>
          <Toolbar meetingId={detail.info.id} isLive={isLive} />
          <SummarySection meetingId={detail.info.id} />
          <Transcript
            segments={detail.segments}
            speakers={detail.speakers}
            meetingId={detail.info.id}
            isLive={isLive}
          />
        </>
      ) : (
        <div className="flex flex-1 items-center justify-center text-sm text-text-muted">
          {detailLoading ? (
            <span className="flex items-center gap-2">
              <Loader2 className="h-4 w-4 animate-spin" />
              {t("meeting.detail.loading")}
            </span>
          ) : (
            t("meeting.live.empty")
          )}
        </div>
      )}
    </div>
  );
};

const TitleEditor: React.FC<{ meetingId: number; title: string }> = ({
  meetingId,
  title,
}) => {
  const { t } = useTranslation();
  const renameLocal = useMeetingUi((s) => s.renameMeetingLocal);
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState(title);

  useEffect(() => {
    if (!editing) setValue(title);
  }, [title, editing]);

  const commit = async () => {
    setEditing(false);
    const next = value.trim();
    if (!next || next === title) {
      setValue(title);
      return;
    }
    renameLocal(meetingId, next);
    const res = await commands.meetingRename(meetingId, next);
    if (res.status === "error") {
      renameLocal(meetingId, title);
      toast.error(t("meeting.errors.rename"), { description: res.error });
    }
  };

  if (editing) {
    return (
      <input
        autoFocus
        value={value}
        placeholder={t("meeting.detail.titlePlaceholder")}
        onChange={(e) => setValue(e.target.value)}
        onBlur={() => void commit()}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
          if (e.key === "Escape") {
            setValue(title);
            setEditing(false);
          }
        }}
        className="min-w-0 flex-1 rounded-lg border border-logo-primary/70 bg-surface-2 px-2 py-0.5 text-sm font-semibold text-text outline-none ring-2 ring-logo-primary/20"
      />
    );
  }

  return (
    <button
      type="button"
      onClick={() => setEditing(true)}
      title={t("meeting.detail.editTitle")}
      className="min-w-0 flex-1 cursor-text truncate rounded-lg px-2 py-0.5 text-start text-sm font-semibold tracking-tight transition-colors hover:bg-text/5"
    >
      {title || t("meeting.untitled")}
    </button>
  );
};

const Toolbar: React.FC<{ meetingId: number; isLive: boolean }> = ({
  meetingId,
  isLive,
}) => {
  const { t } = useTranslation();
  const summarize = useMeetingUi((s) => s.summarize);
  const summarizing = useMeetingUi((s) => s.summarizing);
  const refreshLlm = useMeetingUi((s) => s.refreshLlm);
  const setStore = useMeetingUi((s) => s.set);
  const hasSegments = useMeetingUi(
    (s) => (s.detail?.segments.some((x) => x.text.trim()) ?? false) === true,
  );
  const [exportOpen, setExportOpen] = useState(false);
  const exportRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!exportOpen) return;
    const onDown = (e: MouseEvent) => {
      if (!exportRef.current?.contains(e.target as Node)) setExportOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [exportOpen]);

  const doExport = async (format: MeetingExportFormat) => {
    setExportOpen(false);
    const res = await commands.meetingExport(meetingId, format);
    if (res.status === "error") {
      toast.error(t("meeting.errors.export"), { description: res.error });
      return;
    }
    const path = res.data;
    toast.success(t("meeting.detail.exported"), {
      description: path,
      action: {
        label: t("meeting.detail.copyPath"),
        onClick: () => {
          void navigator.clipboard
            .writeText(path)
            .then(() => toast(t("meeting.detail.pathCopied")));
        },
      },
    });
  };

  const onSummary = async () => {
    setStore({ summaryOpenFor: meetingId, summaryError: null });
    const llm = await refreshLlm();
    if (llm?.installed) {
      void summarize(meetingId);
    } else {
      setStore({ pendingSummaryId: meetingId });
    }
  };

  return (
    <div className="flex h-12 items-center gap-2 border-b border-line px-3">
      <div className="relative" ref={exportRef}>
        <button
          type="button"
          className={iconBtn}
          onClick={() => setExportOpen((v) => !v)}
          aria-expanded={exportOpen}
        >
          <Download className="h-3.5 w-3.5" />
          {t("meeting.detail.export")}
          <ChevronDown className="h-3 w-3 opacity-70" />
        </button>
        {exportOpen && (
          <div className="pf-dialog-in absolute start-0 top-full z-20 mt-1.5 w-48 overflow-hidden rounded-xl border border-text/10 bg-surface-2 p-1 shadow-[0_16px_40px_-12px_rgba(0,0,0,0.6)]">
            {EXPORT_FORMATS.map((f) => (
              <button
                key={f}
                type="button"
                onClick={() => void doExport(f)}
                className="block w-full cursor-pointer rounded-xl px-3 py-1.5 text-start text-xs transition-colors hover:bg-text/6"
              >
                {t(`meeting.detail.formats.${f}`)}
              </button>
            ))}
          </div>
        )}
      </div>
      {/* Summaries need the whole transcript: only after the meeting stops.
          The tooltip sits on a wrapper because disabled buttons get no hover. */}
      <span
        className="inline-flex"
        title={
          isLive ? t("meeting.summary.afterStop") : t("meeting.summary.button")
        }
      >
        <button
          type="button"
          className={iconBtn}
          onClick={() => void onSummary()}
          disabled={summarizing != null || isLive || !hasSegments}
        >
          {summarizing === meetingId ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
          ) : (
            <Bot className="h-3.5 w-3.5 text-logo-primary" />
          )}
          {t("meeting.summary.button")}
        </button>
      </span>
    </div>
  );
};

const SummarySection: React.FC<{ meetingId: number }> = ({ meetingId }) => {
  const { t } = useTranslation();
  const summary = useMeetingUi((s) =>
    s.detail?.info.id === meetingId ? s.detail.info.summary : null,
  );
  const openFor = useMeetingUi((s) => s.summaryOpenFor);
  const summarizing = useMeetingUi((s) => s.summarizing);
  const summaryError = useMeetingUi((s) => s.summaryError);
  const llm = useMeetingUi((s) => s.llm);
  const llmInstalling = useMeetingUi((s) => s.llmInstalling);
  const progress = useMeetingUi((s) => s.llmProgress);
  const summarize = useMeetingUi((s) => s.summarize);
  const setStore = useMeetingUi((s) => s.set);

  const expanded = openFor === meetingId;
  const busy = summarizing === meetingId;
  const needModel = expanded && !busy && !summary && llm && !llm.installed;

  if (!summary && !expanded) return null;

  const install = async () => {
    setStore({ llmInstalling: true, pendingSummaryId: meetingId });
    const res = await commands.llmInstall().catch((e) => ({
      status: "error" as const,
      error: String(e),
    }));
    if (res.status === "error") {
      setStore({ llmInstalling: false, llmProgress: null });
      toast.error(t("meeting.summary.installFailed"), {
        description: res.error,
      });
    }
  };

  const pct =
    progress && progress.total > 0
      ? Math.min(100, Math.round((progress.downloaded / progress.total) * 100))
      : null;

  return (
    <div className="max-h-[45%] shrink-0 overflow-y-auto border-b border-line bg-gradient-to-b from-logo-primary/[0.08] to-transparent px-3.5 py-3">
      <div className="mb-1.5 flex items-center gap-2">
        <Bot className="h-4 w-4 text-logo-primary" />
        <h3 className="flex-1 text-xs font-semibold tracking-wide text-text uppercase">
          {t("meeting.summary.title")}
        </h3>
        {summary && !busy && (
          <button
            type="button"
            onClick={() => void summarize(meetingId)}
            className="flex cursor-pointer items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-text-muted hover:bg-surface-2 hover:text-text"
          >
            <RefreshCw className="h-3 w-3" />
            {t("meeting.summary.regenerate")}
          </button>
        )}
        {!summary && !busy && (
          <button
            type="button"
            aria-label={t("meeting.dialog.close")}
            onClick={() =>
              setStore({ summaryOpenFor: null, pendingSummaryId: null })
            }
            className="cursor-pointer rounded p-0.5 text-text-muted hover:bg-surface-2 hover:text-text"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        )}
      </div>

      {busy ? (
        <div className="flex items-center gap-2 py-1 text-xs text-text-muted">
          <Loader2 className="h-3.5 w-3.5 animate-spin" />
          {t("meeting.summary.running")}
        </div>
      ) : needModel ? (
        <div className="rounded-xl border border-text/[0.08] bg-surface-2 p-3">
          <p className="text-sm font-medium">
            {t("meeting.summary.needModel")}
          </p>
          <p className="mt-0.5 text-xs text-text-muted">
            {t("meeting.summary.needModelHint")}
          </p>
          {llmInstalling ? (
            <div className="mt-2.5">
              <div className="mb-1 flex justify-between text-[11px] text-text-muted">
                <span>
                  {t("meeting.summary.installing")}
                  {progress &&
                    ` · ${t(
                      progress.stage === "server"
                        ? "meeting.summary.stageServer"
                        : "meeting.summary.stageModel",
                    )}`}
                </span>
                <span className="tabular-nums">
                  {pct != null ? `${pct}%` : ""}
                </span>
              </div>
              <div className="h-1.5 overflow-hidden rounded-full bg-line">
                <div
                  className={`h-full rounded-full bg-logo-primary transition-[width] duration-300 ${
                    pct == null ? "w-1/3 animate-pulse" : ""
                  }`}
                  style={pct != null ? { width: `${pct}%` } : undefined}
                />
              </div>
            </div>
          ) : (
            <button
              type="button"
              onClick={() => void install()}
              className="pf-accent mt-2.5 inline-flex h-7 cursor-pointer items-center gap-1.5 rounded-lg px-3 text-xs font-medium transition-transform duration-100 active:scale-[0.97]"
            >
              <Download className="h-3.5 w-3.5" />
              {t("meeting.summary.install")}
            </button>
          )}
        </div>
      ) : summaryError && !summary ? (
        <p className="text-xs text-error">
          {t("meeting.summary.failed")}: {summaryError}
        </p>
      ) : summary ? (
        <div className="meeting-summary text-sm leading-relaxed [&_h1]:mt-2 [&_h1]:text-sm [&_h1]:font-semibold [&_h2]:mt-2 [&_h2]:text-sm [&_h2]:font-semibold [&_h3]:mt-2 [&_h3]:text-sm [&_h3]:font-semibold [&_li]:my-0.5 [&_ol]:list-decimal [&_ol]:ps-5 [&_p]:my-1 [&_strong]:font-semibold [&_ul]:list-disc [&_ul]:ps-5">
          <ReactMarkdown>{summary}</ReactMarkdown>
          {summaryError && (
            <p className="mt-1 text-xs text-error">
              {t("meeting.summary.failed")}: {summaryError}
            </p>
          )}
        </div>
      ) : null}
    </div>
  );
};

const Transcript: React.FC<{
  segments: MeetingSegment[];
  speakers: MeetingSpeaker[];
  meetingId: number;
  isLive: boolean;
}> = ({ segments, speakers, meetingId, isLive }) => {
  const { t } = useTranslation();
  const scrollRef = useRef<HTMLDivElement>(null);
  const [stick, setStick] = useState(true);
  const [hasNew, setHasNew] = useState(false);
  const lastCount = useRef(segments.length);

  // Reset to bottom when switching meetings.
  useEffect(() => {
    setStick(true);
    setHasNew(false);
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [meetingId]);

  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    if (stick) {
      el.scrollTop = el.scrollHeight;
    } else if (segments.length > lastCount.current) {
      setHasNew(true);
    }
    lastCount.current = segments.length;
  }, [segments, stick]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    setStick(atBottom);
    if (atBottom) setHasNew(false);
  };

  const jumpToNew = () => {
    const el = scrollRef.current;
    if (el) el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    setStick(true);
    setHasNew(false);
  };

  const visible = segments.filter((s) => s.text.trim() || !s.is_final);

  return (
    <div className="relative min-h-0 flex-1">
      <div
        ref={scrollRef}
        onScroll={onScroll}
        className="h-full overflow-y-auto px-3.5 py-3 select-text"
      >
        {visible.length === 0 ? (
          <p className="py-8 text-center text-sm text-text-muted">
            {isLive ? t("meeting.live.empty") : t("meeting.detail.empty")}
          </p>
        ) : (
          <ul className="flex flex-col gap-3">
            {visible.map((seg) => (
              <SegmentRow
                key={seg.id}
                segment={seg}
                speaker={findSpeaker(speakers, seg.speaker_id)}
                meetingId={meetingId}
              />
            ))}
          </ul>
        )}
      </div>
      {!stick && (hasNew || isLive) && (
        <button
          type="button"
          onClick={jumpToNew}
          className="absolute bottom-3 left-1/2 flex -translate-x-1/2 cursor-pointer items-center gap-1 rounded-full border border-text/10 bg-surface-2/85 px-3 py-1 text-xs font-medium shadow-[0_10px_28px_-8px_rgba(0,0,0,0.7)] backdrop-blur-xl transition-[border-color,transform] duration-150 hover:border-text/20 active:scale-[0.97]"
        >
          <ArrowDown className="h-3.5 w-3.5" />
          {t("meeting.live.toNew")}
        </button>
      )}
    </div>
  );
};

const SegmentRow: React.FC<{
  segment: MeetingSegment;
  speaker: MeetingSpeaker | undefined;
  meetingId: number;
}> = React.memo(({ segment, speaker, meetingId }) => {
  const { t } = useTranslation();
  const tentative = !segment.is_final;
  return (
    <li className="flex gap-2.5">
      <span className="w-11 shrink-0 pt-0.5 text-end font-mono text-[11px] text-text-muted tabular-nums">
        {formatClock(segment.t0_ms)}
      </span>
      <div className="min-w-0 flex-1">
        <SpeakerChip
          speaker={speaker}
          source={segment.source}
          meetingId={meetingId}
        />
        <p
          className={`mt-0.5 text-sm leading-snug break-words ${
            tentative ? "text-text-muted italic" : "text-text"
          }`}
        >
          {segment.text.trim() || "…"}
        </p>
      </div>
      {tentative && (
        <span className="sr-only">{t("meeting.state.recording")}</span>
      )}
    </li>
  );
});
SegmentRow.displayName = "SegmentRow";

const SpeakerChip: React.FC<{
  speaker: MeetingSpeaker | undefined;
  source: MeetingSegment["source"];
  meetingId: number;
}> = ({ speaker, source, meetingId }) => {
  const { t } = useTranslation();
  const renameLocal = useMeetingUi((s) => s.renameSpeakerLocal);
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState("");

  const fallback =
    source === "microphone"
      ? t("meeting.speaker.me")
      : t("meeting.speaker.other", { n: 1 });
  const display = speaker ? (speaker.name ?? speaker.label) : fallback;
  const color = speakerColor(
    speaker?.color_index ?? (source === "microphone" ? 0 : 1),
  );

  const commit = async () => {
    setEditing(false);
    if (!speaker) return;
    const next = value.trim();
    const prev = speaker.name;
    if (next === (prev ?? "") || (!prev && next === speaker.label)) return;
    renameLocal(speaker.id, next);
    const res = await commands.meetingRenameSpeaker(
      meetingId,
      speaker.id,
      next,
    );
    if (res.status === "error") {
      renameLocal(speaker.id, prev ?? "");
      toast.error(t("meeting.errors.rename"), { description: res.error });
    }
  };

  if (editing && speaker) {
    return (
      <input
        autoFocus
        value={value}
        placeholder={speaker.label}
        aria-label={t("meeting.detail.renameSpeaker")}
        onChange={(e) => setValue(e.target.value)}
        onFocus={(e) => e.currentTarget.select()}
        onBlur={() => void commit()}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
          if (e.key === "Escape") setEditing(false);
        }}
        className="w-40 rounded-full border bg-surface-2 px-2 py-0 text-xs font-semibold outline-none"
        style={{ borderColor: color, color }}
      />
    );
  }

  return (
    <button
      type="button"
      disabled={!speaker}
      onClick={() => {
        setValue(speaker?.name ?? speaker?.label ?? "");
        setEditing(true);
      }}
      title={speaker ? t("meeting.panel.speakerHint") : undefined}
      className="inline-flex max-w-full items-center gap-1.5 rounded-full px-2 py-0 text-xs font-semibold enabled:cursor-pointer enabled:hover:brightness-125"
      style={{ color, backgroundColor: `${color}26` }}
    >
      <span
        className="h-1.5 w-1.5 shrink-0 rounded-full"
        style={{ backgroundColor: color }}
      />
      <span className="truncate">{display}</span>
    </button>
  );
};
