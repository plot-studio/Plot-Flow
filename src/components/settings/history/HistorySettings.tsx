import React, {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { readFile } from "@tauri-apps/plugin-fs";
import {
  Check,
  Copy,
  FolderOpen,
  Loader2,
  RotateCcw,
  Star,
  Trash2,
} from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  commands,
  events,
  type HistoryEntry,
  type HistoryUpdatePayload,
} from "@/bindings";
import { useOsType } from "@/hooks/useOsType";
import { PRESS_SCALE, SPRING, SPRING_SNAPPY } from "@/lib/motion";
import { AudioPlayer, AudioPlayerGroup } from "../../ui/AudioPlayer";

const IconButton: React.FC<{
  onClick: () => void;
  title: string;
  disabled?: boolean;
  active?: boolean;
  children: React.ReactNode;
}> = ({ onClick, title, disabled, active, children }) => (
  <motion.button
    type="button"
    onClick={onClick}
    disabled={disabled}
    whileTap={disabled ? undefined : { scale: 0.9 }}
    transition={SPRING_SNAPPY}
    className={`flex h-7 w-7 cursor-pointer items-center justify-center rounded-md transition-colors duration-150 disabled:cursor-not-allowed disabled:text-text/20 ${
      active
        ? "text-logo-primary hover:bg-logo-primary/10"
        : "text-text-muted hover:bg-text/[0.07] hover:text-text"
    }`}
    title={title}
    aria-label={title}
  >
    {children}
  </motion.button>
);

const PAGE_SIZE = 30;

const dayKey = (seconds: number) => {
  const d = new Date(seconds * 1000);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
};

interface HistorySettingsProps {
  /** Rendered at the right of the page header (e.g. live status chips). */
  headerAccessory?: React.ReactNode;
}

export const HistorySettings: React.FC<HistorySettingsProps> = ({
  headerAccessory,
}) => {
  const { t, i18n } = useTranslation();
  const osType = useOsType();
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [hasMore, setHasMore] = useState(true);
  const sentinelRef = useRef<HTMLDivElement>(null);
  const entriesRef = useRef<HistoryEntry[]>([]);
  const loadingRef = useRef(false);

  // Keep ref in sync for use in IntersectionObserver callback
  useEffect(() => {
    entriesRef.current = entries;
  }, [entries]);

  const loadPage = useCallback(async (cursor?: number) => {
    const isFirstPage = cursor === undefined;
    if (!isFirstPage && loadingRef.current) return;
    loadingRef.current = true;

    if (isFirstPage) setLoading(true);

    try {
      const result = await commands.getHistoryEntries(
        cursor ?? null,
        PAGE_SIZE,
      );
      if (result.status === "ok") {
        const { entries: newEntries, has_more } = result.data;
        setEntries((prev) =>
          isFirstPage ? newEntries : [...prev, ...newEntries],
        );
        setHasMore(has_more);
      }
    } catch (error) {
      console.error("Failed to load history entries:", error);
    } finally {
      setLoading(false);
      loadingRef.current = false;
    }
  }, []);

  // Initial load
  useEffect(() => {
    loadPage();
  }, [loadPage]);

  // Infinite scroll via IntersectionObserver
  useEffect(() => {
    if (loading) return;

    const sentinel = sentinelRef.current;
    if (!sentinel || !hasMore) return;

    const observer = new IntersectionObserver(
      (observerEntries) => {
        const first = observerEntries[0];
        if (first.isIntersecting) {
          const lastEntry = entriesRef.current[entriesRef.current.length - 1];
          if (lastEntry) {
            loadPage(lastEntry.id);
          }
        }
      },
      { threshold: 0 },
    );

    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [loading, hasMore, loadPage]);

  // Listen for new entries added from the transcription pipeline
  useEffect(() => {
    const unlisten = events.historyUpdatePayload.listen((event) => {
      const payload: HistoryUpdatePayload = event.payload;
      if (payload.action === "added") {
        setEntries((prev) => [payload.entry, ...prev]);
      } else if (payload.action === "updated") {
        setEntries((prev) =>
          prev.map((e) => (e.id === payload.entry.id ? payload.entry : e)),
        );
      }
      // "deleted" and "toggled" are handled by optimistic updates only,
      // so we intentionally ignore them here to avoid double-mutation.
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const toggleSaved = async (id: number) => {
    // Optimistic update
    setEntries((prev) =>
      prev.map((e) => (e.id === id ? { ...e, saved: !e.saved } : e)),
    );
    try {
      const result = await commands.toggleHistoryEntrySaved(id);
      if (result.status !== "ok") {
        // Revert on failure
        setEntries((prev) =>
          prev.map((e) => (e.id === id ? { ...e, saved: !e.saved } : e)),
        );
      }
    } catch (error) {
      console.error("Failed to toggle saved status:", error);
      // Revert on failure
      setEntries((prev) =>
        prev.map((e) => (e.id === id ? { ...e, saved: !e.saved } : e)),
      );
    }
  };

  const copyToClipboard = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
    } catch (error) {
      console.error("Failed to copy to clipboard:", error);
    }
  };

  const getAudioUrl = useCallback(
    async (fileName: string) => {
      try {
        const result = await commands.getAudioFilePath(fileName);
        if (result.status === "ok") {
          if (osType === "linux") {
            const fileData = await readFile(result.data);
            const blob = new Blob([fileData], { type: "audio/wav" });
            return URL.createObjectURL(blob);
          }
          return convertFileSrc(result.data, "asset");
        }
        return null;
      } catch (error) {
        console.error("Failed to get audio file path:", error);
        return null;
      }
    },
    [osType],
  );

  const deleteAudioEntry = async (id: number) => {
    // Optimistically remove
    setEntries((prev) => prev.filter((e) => e.id !== id));
    try {
      const result = await commands.deleteHistoryEntry(id);
      if (result.status !== "ok") {
        // Reload on failure
        loadPage();
      }
    } catch (error) {
      console.error("Failed to delete entry:", error);
      loadPage();
    }
  };

  const retryHistoryEntry = async (id: number) => {
    const result = await commands.retryHistoryEntryTranscription(id);
    if (result.status !== "ok") {
      throw new Error(String(result.error));
    }
  };

  const openRecordingsFolder = async () => {
    try {
      const result = await commands.openRecordingsFolder();
      if (result.status !== "ok") {
        throw new Error(String(result.error));
      }
    } catch (error) {
      console.error("Failed to open recordings folder:", error);
    }
  };

  // Group consecutive entries by calendar day (entries arrive newest first).
  const groups = useMemo(() => {
    const out: { key: string; label: string; items: HistoryEntry[] }[] = [];
    const now = new Date();
    const todayKey = dayKey(now.getTime() / 1000);
    const yesterday = new Date(now);
    yesterday.setDate(now.getDate() - 1);
    const yesterdayKey = dayKey(yesterday.getTime() / 1000);
    for (const entry of entries) {
      const key = dayKey(entry.timestamp);
      let group = out[out.length - 1];
      if (!group || group.key !== key) {
        const d = new Date(entry.timestamp * 1000);
        const label =
          key === todayKey
            ? t("home.today")
            : key === yesterdayKey
              ? t("home.yesterday")
              : new Intl.DateTimeFormat(i18n.language, {
                  day: "numeric",
                  month: "long",
                  year:
                    d.getFullYear() === now.getFullYear()
                      ? undefined
                      : "numeric",
                }).format(d);
        group = { key, label, items: [] };
        out.push(group);
      }
      group.items.push(entry);
    }
    return out;
  }, [entries, i18n.language, t]);

  let content: React.ReactNode;

  if (loading) {
    content = (
      <div className="flex items-center gap-2 px-1 py-6 text-[13px] text-text-muted">
        <Loader2 className="h-3.5 w-3.5 animate-spin" />
        {t("settings.history.loading")}
      </div>
    );
  } else if (entries.length === 0) {
    content = (
      <p className="px-1 py-10 text-[13px] text-text-muted">
        {t("home.empty")}
      </p>
    );
  } else {
    content = (
      <AudioPlayerGroup>
        <div className="flex flex-col gap-6">
          <AnimatePresence initial={false}>
            {groups.map((group) => (
              <motion.section
                key={group.key}
                layout="position"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                transition={SPRING}
                className="flex flex-col gap-2"
              >
                <h3 className="pf-label px-1">{group.label}</h3>
                <div className="pf-card pf-rows overflow-hidden">
                  <AnimatePresence initial={false}>
                    {group.items.map((entry) => (
                      <motion.div
                        key={entry.id}
                        layout="position"
                        initial={{ opacity: 0, height: 0 }}
                        animate={{ opacity: 1, height: "auto" }}
                        exit={{ opacity: 0, height: 0 }}
                        transition={SPRING}
                        className="overflow-hidden"
                      >
                        <HistoryEntryComponent
                          entry={entry}
                          onToggleSaved={() => toggleSaved(entry.id)}
                          onCopyText={() =>
                            copyToClipboard(entry.transcription_text)
                          }
                          getAudioUrl={getAudioUrl}
                          deleteAudio={deleteAudioEntry}
                          retryTranscription={retryHistoryEntry}
                        />
                      </motion.div>
                    ))}
                  </AnimatePresence>
                </div>
              </motion.section>
            ))}
          </AnimatePresence>
        </div>
        {/* Sentinel for infinite scroll */}
        <div ref={sentinelRef} className="h-1" />
      </AudioPlayerGroup>
    );
  }

  return (
    <section className="w-full">
      <header className="mb-5 flex items-center justify-between gap-4 px-1">
        <h1 className="pf-title">{t("home.title")}</h1>
        <div className="flex items-center gap-1.5">
          {headerAccessory}
          <motion.button
            type="button"
            onClick={openRecordingsFolder}
            whileTap={{ scale: PRESS_SCALE }}
            transition={SPRING_SNAPPY}
            title={t("settings.history.openFolder")}
            aria-label={t("settings.history.openFolder")}
            className="flex h-7 w-7 cursor-pointer items-center justify-center rounded-md text-text-muted transition-colors duration-150 hover:bg-text/[0.07] hover:text-text"
          >
            <FolderOpen className="h-4 w-4" strokeWidth={1.8} />
          </motion.button>
        </div>
      </header>
      {content}
    </section>
  );
};

interface HistoryEntryProps {
  entry: HistoryEntry;
  onToggleSaved: () => void;
  onCopyText: () => void;
  getAudioUrl: (fileName: string) => Promise<string | null>;
  deleteAudio: (id: number) => Promise<void>;
  retryTranscription: (id: number) => Promise<void>;
}

const HistoryEntryComponent: React.FC<HistoryEntryProps> = ({
  entry,
  onToggleSaved,
  onCopyText,
  getAudioUrl,
  deleteAudio,
  retryTranscription,
}) => {
  const { t, i18n } = useTranslation();
  const [showCopied, setShowCopied] = useState(false);
  const [retrying, setRetrying] = useState(false);

  const hasTranscription = entry.transcription_text.trim().length > 0;

  const handleLoadAudio = useCallback(
    () => getAudioUrl(entry.file_name),
    [getAudioUrl, entry.file_name],
  );

  const handleCopyText = () => {
    if (!hasTranscription) {
      return;
    }

    onCopyText();
    setShowCopied(true);
    setTimeout(() => setShowCopied(false), 2000);
  };

  const handleDeleteEntry = async () => {
    try {
      await deleteAudio(entry.id);
    } catch (error) {
      console.error("Failed to delete entry:", error);
      toast.error(t("settings.history.deleteError"));
    }
  };

  const handleRetranscribe = async () => {
    try {
      setRetrying(true);
      await retryTranscription(entry.id);
    } catch (error) {
      console.error("Failed to re-transcribe:", error);
      toast.error(t("settings.history.retranscribeError"));
    } finally {
      setRetrying(false);
    }
  };

  const time = new Intl.DateTimeFormat(i18n.language, {
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(entry.timestamp * 1000));

  return (
    <div className="group/row flex flex-col gap-2.5 px-4 pt-3.5 pb-3 transition-colors duration-150 hover:bg-text/[0.018]">
      <p
        className={`text-[14px] leading-[1.6] tracking-[-0.006em] ${
          retrying
            ? ""
            : hasTranscription
              ? "cursor-text whitespace-pre-wrap break-words text-text select-text"
              : "text-text-muted"
        }`}
        style={
          retrying
            ? { animation: "transcribe-pulse 3s ease-in-out infinite" }
            : undefined
        }
      >
        {retrying && (
          <style>{`
            @keyframes transcribe-pulse {
              0%, 100% { color: color-mix(in srgb, var(--color-text) 40%, transparent); }
              50% { color: color-mix(in srgb, var(--color-text) 90%, transparent); }
            }
          `}</style>
        )}
        {retrying
          ? t("settings.history.transcribing")
          : hasTranscription
            ? entry.transcription_text
            : t("settings.history.transcriptionFailed")}
      </p>

      <div className="flex h-7 items-center gap-3">
        <span className="flex shrink-0 items-center gap-1.5 text-[12px] text-text-muted tabular-nums">
          {time}
          {entry.saved && (
            <Star
              className="h-3 w-3 text-logo-primary"
              fill="currentColor"
              strokeWidth={0}
              aria-hidden="true"
            />
          )}
        </span>
        <AudioPlayer
          onLoadRequest={handleLoadAudio}
          className="max-w-[240px] flex-1"
        />
        <div className="ms-auto flex items-center gap-0.5 opacity-0 transition-opacity duration-150 group-hover/row:opacity-100 focus-within:opacity-100">
          <IconButton
            onClick={handleCopyText}
            disabled={!hasTranscription || retrying}
            title={t("settings.history.copyToClipboard")}
          >
            {showCopied ? (
              <Check width={15} height={15} />
            ) : (
              <Copy width={15} height={15} />
            )}
          </IconButton>
          <IconButton
            onClick={onToggleSaved}
            disabled={retrying}
            active={entry.saved}
            title={
              entry.saved
                ? t("settings.history.unsave")
                : t("settings.history.save")
            }
          >
            <Star
              width={15}
              height={15}
              fill={entry.saved ? "currentColor" : "none"}
            />
          </IconButton>
          <IconButton
            onClick={handleRetranscribe}
            disabled={retrying}
            title={t("settings.history.retranscribe")}
          >
            <RotateCcw
              width={15}
              height={15}
              style={
                retrying
                  ? { animation: "spin 1s linear infinite reverse" }
                  : undefined
              }
            />
          </IconButton>
          <IconButton
            onClick={handleDeleteEntry}
            disabled={retrying}
            title={t("settings.history.delete")}
          >
            <Trash2 width={15} height={15} />
          </IconButton>
        </div>
      </div>
    </div>
  );
};
