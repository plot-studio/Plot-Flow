import { create } from "zustand";
import {
  commands,
  type LlmStatus,
  type MeetingDetail,
  type MeetingInfo,
  type MeetingSegment,
  type MeetingSpeaker,
  type MeetingStatus,
} from "@/bindings";

export interface LlmInstallProgress {
  stage: "server" | "model";
  downloaded: number;
  total: number;
}

/** Width the native window gains when the panel is open (see meeting_set_panel). */
export const MEETING_PANEL_WIDTH = 460;
const PANEL_EXIT_MS = 220;

interface MeetingUiState {
  /** Latest recorder snapshot (null until the first meeting_status answer). */
  status: MeetingStatus | null;
  panelOpen: boolean;
  /** Meeting shown in the panel: the live one or a past one. */
  detail: MeetingDetail | null;
  detailLoading: boolean;
  meetings: MeetingInfo[] | null;
  listError: boolean;

  llm: LlmStatus | null;
  llmProgress: LlmInstallProgress | null;
  llmInstalling: boolean;
  summarizing: number | null;
  summaryError: string | null;
  /** Meeting whose summary was requested before the model was installed. */
  pendingSummaryId: number | null;
  /** Meeting whose summary card is expanded (install card / spinner / error). */
  summaryOpenFor: number | null;

  summarize: (meetingId: number) => Promise<void>;
  setStatus: (status: MeetingStatus) => void;
  setPanel: (open: boolean) => Promise<void>;
  openMeeting: (meetingId: number) => Promise<void>;
  reloadDetail: () => Promise<void>;
  refreshList: () => Promise<void>;
  upsertSegment: (segment: MeetingSegment) => void;
  applyFinished: (info: MeetingInfo) => void;
  renameSpeakerLocal: (speakerId: number, name: string) => void;
  renameMeetingLocal: (meetingId: number, title: string) => void;
  removeMeetingLocal: (meetingId: number) => void;
  setSummary: (meetingId: number, summary: string) => void;
  refreshLlm: () => Promise<LlmStatus | null>;
  set: (partial: Partial<MeetingUiState>) => void;
}

let panelSeq = 0;
let detailSeq = 0;

export const useMeetingUi = create<MeetingUiState>((set, get) => ({
  status: null,
  panelOpen: false,
  detail: null,
  detailLoading: false,
  meetings: null,
  listError: false,
  llm: null,
  llmProgress: null,
  llmInstalling: false,
  summarizing: null,
  summaryError: null,
  pendingSummaryId: null,
  summaryOpenFor: null,

  set: (partial) => set(partial),

  summarize: async (meetingId) => {
    if (get().summarizing != null) return;
    set({
      summarizing: meetingId,
      summaryError: null,
      summaryOpenFor: meetingId,
      pendingSummaryId: null,
    });
    try {
      const res = await commands.meetingSummarize(meetingId);
      if (res.status === "ok") get().setSummary(meetingId, res.data);
      else set({ summaryError: res.error });
    } catch (e) {
      set({ summaryError: String(e) });
    } finally {
      set({ summarizing: null });
    }
  },

  setStatus: (status) => set({ status }),

  setPanel: async (open) => {
    const seq = ++panelSeq;
    if (open) {
      if (get().panelOpen) return;
      // Widen the native window first so the main column never gets squeezed.
      const res = await commands.meetingSetPanel(true).catch((e) => ({
        status: "error" as const,
        error: String(e),
      }));
      if (res.status === "error") {
        console.warn("meeting_set_panel(true) failed:", res.error);
      }
      if (seq === panelSeq) set({ panelOpen: true });
    } else {
      if (!get().panelOpen) return;
      set({ panelOpen: false });
      // Let the slide-out animation finish before the window shrinks.
      await new Promise((r) => setTimeout(r, PANEL_EXIT_MS));
      if (seq !== panelSeq) return;
      const res = await commands.meetingSetPanel(false).catch((e) => ({
        status: "error" as const,
        error: String(e),
      }));
      if (res.status === "error") {
        console.warn("meeting_set_panel(false) failed:", res.error);
      }
    }
  },

  openMeeting: async (meetingId) => {
    const seq = ++detailSeq;
    const current = get().detail;
    set({
      detailLoading: current?.info.id !== meetingId,
      summaryError: null,
    });
    if (current?.info.id !== meetingId) set({ detail: null });
    void get().setPanel(true);
    const res = await commands.meetingGet(meetingId);
    if (seq !== detailSeq) return;
    if (res.status === "ok") {
      set({
        detail: mergeDetail(get().detail, res.data),
        detailLoading: false,
      });
    } else {
      set({ detailLoading: false });
      throw new Error(res.error);
    }
  },

  reloadDetail: async () => {
    const id = get().detail?.info.id;
    if (id == null) return;
    const seq = ++detailSeq;
    const res = await commands.meetingGet(id);
    if (seq !== detailSeq || res.status !== "ok") return;
    if (get().detail?.info.id !== id) return;
    set({ detail: mergeDetail(get().detail, res.data) });
  },

  refreshList: async () => {
    const res = await commands.meetingList();
    if (res.status === "ok") {
      const sorted = [...res.data].sort((a, b) => b.started_at - a.started_at);
      set({ meetings: sorted, listError: false });
    } else {
      set({ listError: true });
    }
  },

  upsertSegment: (segment) => {
    const detail = get().detail;
    if (!detail || detail.info.id !== segment.meeting_id) return;
    const segments = detail.segments.slice();
    const idx = segments.findIndex((s) => s.id === segment.id);
    if (idx >= 0) segments[idx] = segment;
    else {
      segments.push(segment);
      if (
        segments.length > 1 &&
        segments[segments.length - 2].t0_ms > segment.t0_ms
      ) {
        segments.sort((a, b) => a.t0_ms - b.t0_ms || a.id - b.id);
      }
    }
    set({ detail: { ...detail, segments } });
  },

  applyFinished: (info) => {
    const { detail, meetings } = get();
    if (detail && detail.info.id === info.id) {
      set({ detail: { ...detail, info } });
    }
    if (meetings) {
      const rest = meetings.filter((m) => m.id !== info.id);
      set({
        meetings: [info, ...rest].sort((a, b) => b.started_at - a.started_at),
      });
    }
  },

  renameSpeakerLocal: (speakerId, name) => {
    const detail = get().detail;
    if (!detail) return;
    set({
      detail: {
        ...detail,
        speakers: detail.speakers.map((s) =>
          s.id === speakerId ? { ...s, name: name || null } : s,
        ),
      },
    });
  },

  renameMeetingLocal: (meetingId, title) => {
    const { detail, meetings } = get();
    if (detail && detail.info.id === meetingId) {
      set({ detail: { ...detail, info: { ...detail.info, title } } });
    }
    if (meetings) {
      set({
        meetings: meetings.map((m) =>
          m.id === meetingId ? { ...m, title } : m,
        ),
      });
    }
  },

  removeMeetingLocal: (meetingId) => {
    const { detail, meetings } = get();
    if (meetings) set({ meetings: meetings.filter((m) => m.id !== meetingId) });
    if (detail && detail.info.id === meetingId) {
      set({ detail: null });
      void get().setPanel(false);
    }
  },

  setSummary: (meetingId, summary) => {
    const { detail, meetings } = get();
    if (detail && detail.info.id === meetingId) {
      set({ detail: { ...detail, info: { ...detail.info, summary } } });
    }
    if (meetings) {
      set({
        meetings: meetings.map((m) =>
          m.id === meetingId ? { ...m, summary } : m,
        ),
      });
    }
  },

  refreshLlm: async () => {
    const res = await commands.llmStatus().catch(() => null);
    if (res && res.status === "ok") {
      set({ llm: res.data, llmInstalling: res.data.installing });
      return res.data;
    }
    return null;
  },
}));

/** Keep segments that arrived over events but are not yet in the fetched copy. */
function mergeDetail(
  local: MeetingDetail | null,
  fetched: MeetingDetail,
): MeetingDetail {
  if (!local || local.info.id !== fetched.info.id) return fetched;
  const byId = new Map<number, MeetingSegment>();
  for (const s of fetched.segments) byId.set(s.id, s);
  for (const s of local.segments) {
    const f = byId.get(s.id);
    if (!f || (!f.is_final && s.is_final)) byId.set(s.id, s);
  }
  const segments = [...byId.values()].sort(
    (a, b) => a.t0_ms - b.t0_ms || a.id - b.id,
  );
  return { ...fetched, segments };
}

export const SPEAKER_COLORS = [
  "#96a36c",
  "#6ca3a0",
  "#c08a5b",
  "#8c84c9",
  "#c46f8a",
  "#5f93c9",
  "#b9a24a",
  "#7fb37a",
];

export const speakerColor = (index: number | undefined | null) =>
  SPEAKER_COLORS[
    (((index ?? 0) % SPEAKER_COLORS.length) + SPEAKER_COLORS.length) %
      SPEAKER_COLORS.length
  ];

export const findSpeaker = (
  speakers: MeetingSpeaker[],
  id: number | null,
): MeetingSpeaker | undefined =>
  id == null ? undefined : speakers.find((s) => s.id === id);

export const formatClock = (ms: number) => {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
};

export const isActiveState = (status: MeetingStatus | null) =>
  !!status && status.state !== "idle";
