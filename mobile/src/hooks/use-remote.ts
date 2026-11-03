import {
  useCallback,
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { remoteClient } from "@/lib/remote-client";
import type {
  CommandMap,
  CommandType,
  SessionDetail,
  SessionSummary,
  Workspace,
} from "@/lib/types";

interface Selection {
  projectId: string;
  sessionId: string;
  page: "sessions" | "conversation";
}
interface Data {
  workspaces: Workspace[];
  sessions: SessionSummary[];
  detail: SessionDetail | null;
  projectId: string;
}
const emptyData: Data = {
  workspaces: [],
  sessions: [],
  detail: null,
  projectId: "",
};

function savedSelection(): Selection {
  try {
    return {
      projectId: localStorage.getItem("fintwind-project") || "",
      sessionId: localStorage.getItem("fintwind-session") || "",
      page: localStorage.getItem("fintwind-page") === "conversation" && localStorage.getItem("fintwind-session") ? "conversation" : "sessions",
    };
  } catch {
    return { projectId: "", sessionId: "", page: "sessions" };
  }
}

function saveSelection(key: string, value: string) {
  try {
    if (value) localStorage.setItem(key, value);
    else localStorage.removeItem(key);
  } catch {
    /* Selection is optional; pairing handles unavailable storage. */
  }
}

export function useRemote() {
  const client = useSyncExternalStore(
    remoteClient.subscribe,
    remoteClient.getSnapshot,
  );
  const [selection, setSelection] = useState<Selection>({
    projectId: "",
    sessionId: "",
    page: "sessions",
  });
  const [data, setData] = useState<Data>(emptyData);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [operationError, setOperationError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [stopping, setStopping] = useState(false);
  const generation = useRef(0);
  const operation = useRef<CommandType | null>(null);
  const stopOperation = useRef(false);
  const currentSelection = useRef(selection);
  const [refreshVersion, setRefreshVersion] = useState(0);
  currentSelection.current = selection;
  const workspaceCache = useRef<{
    at: number;
    data: CommandMap["listWorkspaces"]["result"];
  } | null>(null);

  useEffect(() => {
    remoteClient.start();
    setSelection(savedSelection());
    history.replaceState(
      { fintwindPage: "sessions" },
      "",
      location.pathname + location.search,
    );
    const onPop = (event: PopStateEvent) => {
      generation.current++;
      const next = event.state;
      if (
        next?.fintwindPage === "conversation" &&
        typeof next.sessionId === "string" &&
        typeof next.projectId === "string"
      ) {
        setSelection({
          projectId: next.projectId,
          sessionId: next.sessionId,
          page: "conversation",
        });
      } else setSelection((value) => ({ ...value, page: "sessions" }));
      saveSelection("fintwind-page", next?.fintwindPage === "conversation" ? "conversation" : "sessions");
    };
    const onHide = () => remoteClient.stop();
    const onShow = () => remoteClient.start();
    window.addEventListener("pagehide", onHide);
    window.addEventListener("pageshow", onShow);
    window.addEventListener("popstate", onPop);
    return () => {
      generation.current++;
      window.removeEventListener("popstate", onPop);
      window.removeEventListener("pagehide", onHide);
      window.removeEventListener("pageshow", onShow);
      remoteClient.stop();
    };
  }, []);

  useEffect(() => {
    if (client.connection !== "connected") return;
    const restored = savedSelection();
    setSelection((current) => current.projectId === restored.projectId && current.sessionId === restored.sessionId && current.page === restored.page ? current : restored);
    if (restored.page === "conversation" && history.state?.fintwindPage !== "conversation") {
      history.pushState({ fintwindPage: "conversation", sessionId: restored.sessionId, projectId: restored.projectId }, "");
    }
  }, [client.connection]);

  useEffect(() => {
    if (client.connection !== "connected") {
      workspaceCache.current = null;
      setLoading(false);
      return;
    }
    const epoch = ++generation.current;
    let active = true;
    let fetching = false;
    let unwatch: (() => void) | null = null;
    let streamUnavailable = false;
    let streamFailures = 0;
    let timer = 0;
    const idleInterval = selection.page === "conversation" ? 2500 : 8000;
    let refreshInterval = idleInterval;
    const sessionInterval = (detail: SessionDetail) =>
      ["connecting", "working", "waiting"].includes(detail.status) ? 300 : idleInterval;
    const scheduleRefresh = () => {
      window.clearTimeout(timer);
      if (active && !document.hidden && !unwatch)
        timer = window.setTimeout(() => void refresh(), refreshInterval);
    };
    const stopWatching = () => { unwatch?.(); unwatch = null; };
    const refresh = async () => {
      if (!active || fetching || document.hidden || unwatch) return;
      window.clearTimeout(timer);
      fetching = true;
      try {
        if (selection.page === "conversation" && selection.sessionId) {
          if (!workspaceCache.current || Date.now() - workspaceCache.current.at >= 30000) {
            void remoteClient.command("listWorkspaces", {}).then((result) => {
              if (!active || epoch !== generation.current) return;
              workspaceCache.current = { at: Date.now(), data: result };
              setData((value) => ({ ...value, workspaces: result.workspaces }));
            }).catch(() => { /* Session reads report their own error; workspace labels are optional. */ });
          }
          const detail = await remoteClient.command("getSession", {
            sessionId: selection.sessionId,
          });
          if (!active || epoch !== generation.current) return;
          refreshInterval = sessionInterval(detail);
          setData((value) =>
            value.detail === detail ? value : { ...value, detail },
          );
          setError(null);
          if (!document.hidden && !streamUnavailable && detail.features.includes("sessionDeltas")) {
            unwatch = remoteClient.watchSession(selection.sessionId, (updated) => {
              if (!active || epoch !== generation.current || document.hidden) return;
              refreshInterval = sessionInterval(updated);
              setData((value) => ({ ...value, detail: updated }));
              setError(null);
            }, () => {
              if (!active || epoch !== generation.current) return;
              stopWatching();
              // Retry from a complete snapshot; repeated failures use the old
              // low-frequency protocol rather than a reconnect storm.
              streamUnavailable = ++streamFailures >= 2;
              void refresh();
            });
          }
          return;
        }
        const cached = workspaceCache.current;
        const workspaceData = cached && Date.now() - cached.at < 30000
          ? cached.data : await remoteClient.command("listWorkspaces", {});
        if (!active || epoch !== generation.current) return;
        workspaceCache.current = { at: cached?.data === workspaceData ? cached.at : Date.now(), data: workspaceData };
        if (!active || epoch !== generation.current) return;
        const projectId = workspaceData.workspaces.some(
          (row) => row.id === selection.projectId,
        )
          ? selection.projectId
          : workspaceData.workspaces.find(
              (row) => row.id === workspaceData.selectedProjectId,
            )?.id ||
            workspaceData.workspaces[0]?.id ||
            "";
        if (!projectId) {
          setData({ ...emptyData, workspaces: workspaceData.workspaces });
          setLoading(false);
          return;
        }
        if (projectId !== selection.projectId) {
          saveSelection("fintwind-project", projectId);
          saveSelection("fintwind-session", "");
          saveSelection("fintwind-page", "sessions");
          setSelection((value) => ({
            ...value,
            projectId,
            sessionId: "",
            page: "sessions",
          }));
          return;
        }
        const list = await remoteClient.command("listSessions", { projectId });
        if (!active || epoch !== generation.current) return;
        setData((value) => ({
          workspaces: workspaceData.workspaces,
          sessions: list.sessions,
          detail: value.detail,
          projectId,
        }));
        setError(null);
      } catch (cause) {
        if (active && epoch === generation.current) {
          refreshInterval = idleInterval;
          // A transient read failure must not unmount the transcript and lose
          // its scroll position. Selection changes clear stale content below.
          setError(cause instanceof Error ? cause.message : "无法刷新会话。");
        }
      } finally {
        fetching = false;
        if (active && epoch === generation.current) setLoading(false);
        scheduleRefresh();
      }
    };
    setLoading(true);
    void refresh();
    const onVisible = () => {
      if (document.hidden) { stopWatching(); window.clearTimeout(timer); }
      else { streamUnavailable = false; streamFailures = 0; void refresh(); }
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      active = false;
      stopWatching();
      window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [
    client.connection,
    selection.projectId,
    selection.sessionId,
    selection.page,
    refreshVersion,
  ]);

  const selectWorkspace = useCallback((projectId: string) => {
    generation.current++;
    setError(null);
    setOperationError(null);
    setData((value) => ({ ...value, sessions: [], detail: null, projectId }));
    setSelection({ projectId, sessionId: "", page: "sessions" });
    saveSelection("fintwind-project", projectId);
    saveSelection("fintwind-session", "");
    saveSelection("fintwind-page", "sessions");
  }, []);

  const openSession = useCallback((sessionId: string, targetProjectId?: string) => {
    const projectId = targetProjectId ?? currentSelection.current.projectId;
    setError(null);
    setOperationError(null);
    // A reminder can target the already-open conversation. Invalidating its
    // generation without changing effect dependencies would freeze its stream.
    if (currentSelection.current.page === "conversation" &&
      currentSelection.current.sessionId === sessionId && currentSelection.current.projectId === projectId) return;
    generation.current++;
    if (currentSelection.current.page !== "conversation")
      history.pushState(
        {
          fintwindPage: "conversation",
          sessionId,
          projectId,
        },
        "",
      );
    else history.replaceState({ fintwindPage: "conversation", sessionId, projectId }, "");
    setSelection({ projectId, sessionId, page: "conversation" });
    saveSelection("fintwind-project", projectId);
    saveSelection("fintwind-session", sessionId);
    saveSelection("fintwind-page", "conversation");
  }, []);

  const back = useCallback(() => {
    generation.current++;
    if (history.state?.fintwindPage === "conversation") history.back();
    else setSelection((value) => ({ ...value, page: "sessions" }));
    saveSelection("fintwind-page", "sessions");
  }, []);

  const mutate = useCallback(
    async <K extends CommandType>(type: K, fields: CommandMap[K]["fields"], onFailure?: (message: string) => void) => {
      const stop = type === "stop";
      if (operation.current && !stop) return null;
      if (stopOperation.current) return null;
      if (stop) { stopOperation.current = true; setStopping(true); }
      else { operation.current = type; setBusy(true); }
      setOperationError(null);
      try {
        const result = await remoteClient.command(type, fields);
        return result;
      } catch (cause) {
        const message = cause instanceof Error ? cause.message : "操作失败。";
        if (onFailure) onFailure(message);
        else setOperationError(message);
        return null;
      } finally {
        // A timeout still needs a fresh read to resolve operation identities;
        // unsolicited pushes are not proof of a post-operation snapshot.
        setRefreshVersion((value) => value + 1);
        if (stop) { stopOperation.current = false; setStopping(false); }
        else { operation.current = null; setBusy(false); }
      }
    },
    [],
  );

  const createSession = useCallback(async () => {
    const projectId = currentSelection.current.projectId;
    if (!projectId) return;
    const result = await mutate("createSession", { projectId });
    if (result && currentSelection.current.projectId === projectId)
      openSession(result.sessionId);
  }, [mutate, openSession]);

  return {
    ...client,
    ...selection,
    ...data,
    projectId: selection.projectId,
    detail: data.detail?.id === selection.sessionId ? data.detail : null,
    sessions: data.projectId === selection.projectId ? data.sessions : [],
    loading,
    error: operationError || error || client.error,
    busy,
    stopping,
    selectWorkspace,
    openSession,
    back,
    mutate,
    createSession,
    setError: setOperationError,
    refresh: () => setRefreshVersion((value) => value + 1),
    reconnect: () => remoteClient.reconnect(),
    forget: async () => {
      try {
        await remoteClient.forget();
        generation.current++;
        setData(emptyData);
        setSelection({ projectId: "", sessionId: "", page: "sessions" });
        setError(null);
        setOperationError(null);
      } catch (cause) { setError(cause instanceof Error ? cause.message : "无法忘记配对。"); }
    },
  };
}
