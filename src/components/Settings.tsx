import { useState, useEffect, useRef, useCallback, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  checkAccessibilityPermission,
  requestAccessibilityPermission,
  checkMicrophonePermission,
  requestMicrophonePermission,
} from "tauri-plugin-macos-permissions-api";
import { save as saveDialog, open as openDialog } from "@tauri-apps/plugin-dialog";
import { isEnabled as isAutostartEnabled, enable as enableAutostart, disable as disableAutostart } from "@tauri-apps/plugin-autostart";
import Logo from "./Logo";
import "./Settings.css";

interface ModelInfo {
  name: string;
  displayName: string;
  engine: string;
  architecture: string;
  size: string;
  sizeBytes: number;
  quality: string;
  speed: string;
  description: string;
  downloaded: boolean;
  recommended: boolean;
  advanced: boolean;
  runnable: boolean;
  languages: string[];
  family: string;
  kind: string; // "asr" | "llm"
  custom: boolean; // user-added enhancement model
  rank: number;
}

interface AppSettings {
  model: string;
  output_mode: string;
  hotkey: string;
  recording_mode: string;
  onboarding_complete: boolean;
  append_trailing_space?: boolean;
  start_hidden?: boolean;
  show_overlay?: boolean;
  history_limit?: number;
  microphone?: string;
  audio_feedback?: boolean;
  language?: string;
  silence_threshold?: number;
  theme?: string;
  hide_dock_icon?: boolean;
  live_preview?: boolean;
  show_transcript_in_overlay?: boolean;
  enhance_enabled?: boolean;
  enhance_model?: string;
  enhance_mode?: string;
  enhance_intensity?: string;
  enhance_custom_prompt?: string;
  enhance_voice_commands?: boolean;
  enhance_debug_log?: boolean;
  enhance_max_tokens?: number;
}

const LANGUAGES: [string, string][] = [
  ["auto", "Auto-detect"],
  ["en", "English"],
  ["es", "Spanish"],
  ["fr", "French"],
  ["de", "German"],
  ["it", "Italian"],
  ["pt", "Portuguese"],
  ["nl", "Dutch"],
  ["ru", "Russian"],
  ["zh", "Chinese"],
  ["ja", "Japanese"],
  ["ko", "Korean"],
  ["hi", "Hindi"],
  ["ar", "Arabic"],
];

interface TranscriptEntry {
  id: number;
  text: string;
  raw?: string | null;
  timestamp: number;
}

interface PermissionsStatus {
  accessibility: boolean;
  microphone: boolean;
  model_ready: boolean;
  onboarding_complete: boolean;
  active_model: string;
  process_name: string;
  executable_path: string;
  is_dev: boolean;
}

interface DownloadProgress {
  model: string;
  file?: string;
  fileIndex?: number;
  fileCount?: number;
  bytesReceived?: number;
  totalBytes?: number;
  done?: boolean;
  cancelled?: boolean;
}

interface DownloadState {
  active: boolean;
  currentFile: string;
  fileIndex: number;
  fileCount: number;
  bytesReceived: number;
  totalBytes: number;
  pct: number;
  error?: string;
}

type Tab = "models" | "general" | "enhance" | "history" | "advanced" | "permissions" | "about";
type ModelFilter = "all" | "runnable" | "recommended" | "downloaded";

function formatBytes(b: number) {
  if (b === 0) return "0 B";
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  if (b < 1024 * 1024 * 1024) return `${(b / (1024 * 1024)).toFixed(1)} MB`;
  return `${(b / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

/** "2m ago"-style relative time from a unix-seconds timestamp. */
function relativeTime(unixSecs: number) {
  const diff = Math.max(0, Math.floor(Date.now() / 1000) - unixSecs);
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return new Date(unixSecs * 1000).toLocaleDateString();
}

function hotkeyLabel(hotkey: string) {
  return hotkey
    .replace(/CommandOrControl/g, "⌘")
    .replace(/Command/g, "⌘")
    .replace(/Control/g, "Ctrl")
    .replace(/Shift/g, "⇧")
    .replace(/Option|Alt/g, "⌥")
    .replace(/\+/g, " ");
}

/** Maps a KeyboardEvent.code to the key name tauri-plugin-global-shortcut
 * expects in an accelerator string. Returns null for modifier-only codes. */
function codeToAccelKey(code: string): string | null {
  if (code.startsWith("Key")) return code.slice(3);
  if (code.startsWith("Digit")) return code.slice(5);
  if (code.startsWith("Arrow")) return code;
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  switch (code) {
    case "Space":
      return "Space";
    case "Escape":
      return "Escape";
    case "Backspace":
      return "Backspace";
    case "Tab":
      return "Tab";
    case "Enter":
      return "Return";
    case "Minus":
      return "-";
    case "Equal":
      return "=";
    case "BracketLeft":
      return "[";
    case "BracketRight":
      return "]";
    case "Semicolon":
      return ";";
    case "Quote":
      return "'";
    case "Backslash":
      return "\\";
    case "Comma":
      return ",";
    case "Period":
      return ".";
    case "Slash":
      return "/";
    case "Backquote":
      return "`";
    default:
      return null;
  }
}

function ShortcutRecorder({
  value,
  onSave,
}: {
  value: string;
  onSave: (accel: string) => void;
}) {
  const [recording, setRecording] = useState(false);
  const [checking, setChecking] = useState(false);
  const [conflict, setConflict] = useState(false);

  useEffect(() => {
    if (!recording) return;
    function onKeyDown(e: KeyboardEvent) {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        setRecording(false);
        return;
      }
      const mainKey = codeToAccelKey(e.code);
      if (!mainKey) return; // waiting for a non-modifier key
      const mods: string[] = [];
      if (e.metaKey || e.ctrlKey) mods.push("CommandOrControl");
      if (e.altKey) mods.push("Alt");
      if (e.shiftKey) mods.push("Shift");
      if (mods.length === 0) return; // require at least one modifier

      const accel = [...mods, mainKey].join("+");
      setRecording(false);
      setChecking(true);
      invoke<boolean>("is_shortcut_available", { accel })
        .then((ok) => {
          setConflict(!ok);
          if (ok) onSave(accel);
        })
        .catch(() => setConflict(true))
        .finally(() => setChecking(false));
    }
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [recording, onSave]);

  return (
    <div className="s-field">
      <label className="s-label">Global shortcut</label>
      <button
        type="button"
        className="s-input"
        style={{ textAlign: "left", cursor: "pointer" }}
        onClick={() => {
          setConflict(false);
          setRecording(true);
        }}
      >
        {recording ? "Press a key combo… (Esc to cancel)" : hotkeyLabel(value)}
      </button>
      {checking && <p className="s-help">Checking availability…</p>}
      {conflict && (
        <p className="s-help" style={{ color: "var(--danger)" }}>
          That combo is already in use — try another.
        </p>
      )}
      {!recording && !checking && !conflict && (
        <p className="s-help">Click, then press your shortcut.</p>
      )}
    </div>
  );
}

export default function Settings() {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [perms, setPerms] = useState<PermissionsStatus | null>(null);
  const [saved, setSaved] = useState(false);
  const [tab, setTab] = useState<Tab>("models");
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<ModelFilter>("all");
  const [showAllCatalog, setShowAllCatalog] = useState(true);
  const [downloads, setDownloads] = useState<Record<string, DownloadState>>({});
  const [axBusy, setAxBusy] = useState(false);
  const [history, setHistory] = useState<TranscriptEntry[]>([]);
  const [mics, setMics] = useState<string[]>([]);
  const [catalogTotal, setCatalogTotal] = useState(0);
  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  const [importError, setImportError] = useState("");
  const [copiedId, setCopiedId] = useState<number | null>(null);
  const [showRawId, setShowRawId] = useState<number | null>(null);
  const unlistenRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    isAutostartEnabled().then(setLaunchAtLogin).catch(() => {});
  }, []);

  const toggleLaunchAtLogin = useCallback(async (checked: boolean) => {
    try {
      if (checked) await enableAutostart();
      else await disableAutostart();
      setLaunchAtLogin(checked);
    } catch {
      /* leave state unchanged on failure */
    }
  }, []);

  const exportSettings = useCallback(async () => {
    const path = await saveDialog({
      defaultPath: "openvoice-settings.json",
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!path) return;
    await invoke("export_settings_to", { path });
  }, []);

  const importSettings = useCallback(async () => {
    setImportError("");
    const path = await openDialog({
      multiple: false,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!path || Array.isArray(path)) return;
    try {
      const next = await invoke<AppSettings>("import_settings_from", { path });
      setSettings(next);
    } catch (e) {
      setImportError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const refresh = useCallback(async () => {
    const [m, s, p, h, micList, stats] = await Promise.all([
      invoke<ModelInfo[]>("get_models"),
      invoke<AppSettings>("get_settings"),
      invoke<PermissionsStatus>("get_permissions"),
      invoke<TranscriptEntry[]>("get_history").catch(() => [] as TranscriptEntry[]),
      invoke<string[]>("list_microphones").catch(() => [] as string[]),
      invoke<{ catalogModels: number; totalListed: number }>("catalog_stats").catch(() => ({
        catalogModels: 0,
        totalListed: 0,
      })),
    ]);
    try {
      const ax = await checkAccessibilityPermission();
      p.accessibility = ax;
    } catch {
      /* keep backend value */
    }
    try {
      // Real TCC permission — the backend's `microphone` flag only reflects
      // whether a mic *device* exists, not whether we're allowed to use it,
      // so a denied mic reads as "ready" and recording silently captures
      // silence. Override with the actual permission state.
      p.microphone = await checkMicrophonePermission();
    } catch {
      /* keep backend value */
    }
    setModels(m);
    setSettings({
      append_trailing_space: true,
      show_overlay: true,
      history_limit: 50,
      audio_feedback: false,
      start_hidden: false,
      microphone: "",
      language: "auto",
      silence_threshold: 0.005,
      theme: "system",
      hide_dock_icon: false,
      live_preview: false,
      show_transcript_in_overlay: false,
      enhance_enabled: false,
      enhance_model: "",
      enhance_mode: "clean",
      enhance_intensity: "light",
      enhance_custom_prompt: "",
      enhance_voice_commands: false,
      enhance_debug_log: false,
      enhance_max_tokens: 0,
      ...s,
    });
    setPerms(p);
    setHistory(h);
    setMics(micList);
    setCatalogTotal(stats.totalListed || m.length);
  }, []);

  useEffect(() => {
    const theme = settings?.theme ?? "system";
    const root = document.documentElement;
    if (theme === "system") {
      root.removeAttribute("data-theme");
    } else {
      root.setAttribute("data-theme", theme);
    }
  }, [settings?.theme]);

  useEffect(() => {
    refresh();
    const id = setInterval(() => {
      invoke<PermissionsStatus>("get_permissions")
        .then(async (p) => {
          try {
            p.accessibility = await checkAccessibilityPermission();
          } catch {
            /* ignore */
          }
          try {
            p.microphone = await checkMicrophonePermission();
          } catch {
            /* ignore */
          }
          setPerms(p);
        })
        .catch(() => {});
    }, 1500);
    return () => clearInterval(id);
  }, [refresh]);

  // Proactively trigger the macOS microphone prompt on first launch if it
  // hasn't been granted — otherwise the app records silence with no signal
  // and the user has no idea permission is the problem.
  const micRequestedRef = useRef(false);
  useEffect(() => {
    if (micRequestedRef.current) return;
    checkMicrophonePermission()
      .then((granted) => {
        if (!granted) {
          micRequestedRef.current = true;
          requestMicrophonePermission().catch(() => {});
        }
      })
      .catch(() => {});
  }, []);

  // Tray menu can request a specific tab ("Models…", "History…", "About").
  useEffect(() => {
    const un = listen<string>("settings-navigate", ({ payload }) => {
      if (["models", "general", "history", "advanced", "permissions", "about"].includes(payload)) {
        setTab(payload as Tab);
      }
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    listen<DownloadProgress>("download-progress", ({ payload: p }) => {
      if (!p.model) return;
      setDownloads((prev) => {
        const cur = prev[p.model] ?? {
          active: true,
          currentFile: "",
          fileIndex: 0,
          fileCount: 1,
          bytesReceived: 0,
          totalBytes: 0,
          pct: 0,
        };
        if (p.cancelled) {
          return { ...prev, [p.model]: { ...cur, active: false, error: "Cancelled" } };
        }
        if (p.done) {
          refresh();
          return { ...prev, [p.model]: { ...cur, active: false, pct: 100 } };
        }
        const totalBytes = p.totalBytes ?? cur.totalBytes;
        const bytesReceived = p.bytesReceived ?? cur.bytesReceived;
        const pct =
          totalBytes > 0 ? Math.min(99, Math.round((bytesReceived / totalBytes) * 100)) : 0;
        return {
          ...prev,
          [p.model]: {
            active: true,
            currentFile: p.file ?? cur.currentFile,
            fileIndex: p.fileIndex ?? cur.fileIndex,
            fileCount: p.fileCount ?? cur.fileCount,
            bytesReceived,
            totalBytes,
            pct,
          },
        };
      });
    }).then((un) => {
      if (cancelled) un();
      else unlistenRef.current = un;
    });
    return () => {
      cancelled = true;
      unlistenRef.current?.();
    };
  }, [refresh]);

  const startDownload = useCallback(
    async (modelName: string) => {
      setDownloads((prev) => ({
        ...prev,
        [modelName]: {
          active: true,
          currentFile: "",
          fileIndex: 0,
          fileCount: 1,
          bytesReceived: 0,
          totalBytes: 0,
          pct: 0,
        },
      }));
      try {
        await invoke("download_model", { modelName });
        const model = models.find((m) => m.name === modelName);
        if (settings && model?.kind === "llm") {
          // Enhancement model: select it for enhancement, don't touch the
          // active ASR model.
          const next = { ...settings, enhance_model: modelName };
          setSettings(next);
          await invoke("save_settings", { settings: next });
        } else if (settings && model?.runnable) {
          const next = { ...settings, model: modelName };
          setSettings(next);
          await invoke("save_settings", { settings: next });
        }
        await refresh();
      } catch (e: unknown) {
        const msg = e instanceof Error ? e.message : String(e);
        if (!msg.includes("cancelled")) {
          setDownloads((prev) => ({
            ...prev,
            [modelName]: {
              ...(prev[modelName] ?? {
                active: false,
                currentFile: "",
                fileIndex: 0,
                fileCount: 1,
                bytesReceived: 0,
                totalBytes: 0,
                pct: 0,
              }),
              active: false,
              error: msg,
            },
          }));
        }
      }
    },
    [settings, models, refresh],
  );

  const cancelDownload = useCallback(async () => {
    await invoke("cancel_download");
  }, []);

  const deleteModel = useCallback(
    async (modelName: string) => {
      await invoke("delete_model", { modelName });
      refresh();
    },
    [refresh],
  );

  const removeCustomModel = useCallback(
    async (slug: string) => {
      await invoke("remove_enhancement_model", { slug });
      refresh();
    },
    [refresh],
  );

  // Add-custom-model form state.
  const [showAddModel, setShowAddModel] = useState(false);
  const [addForm, setAddForm] = useState({
    repo: "",
    ggufFile: "",
    family: "qwen2",
    tokenizerRepo: "",
    displayName: "",
  });
  const [ggufChoices, setGgufChoices] = useState<string[]>([]);
  const [addBusy, setAddBusy] = useState(false);
  const [addError, setAddError] = useState("");

  const fetchGgufFiles = useCallback(async () => {
    setAddError("");
    setGgufChoices([]);
    if (!addForm.repo.trim()) return;
    setAddBusy(true);
    try {
      const files = await invoke<string[]>("fetch_repo_gguf_files", { repo: addForm.repo.trim() });
      setGgufChoices(files);
      if (files.length === 0) {
        setAddError("No .gguf files found in that repository.");
      } else if (!addForm.ggufFile) {
        setAddForm((f) => ({ ...f, ggufFile: files[0] }));
      }
    } catch (e: unknown) {
      setAddError(e instanceof Error ? e.message : String(e));
    } finally {
      setAddBusy(false);
    }
  }, [addForm.repo, addForm.ggufFile]);

  const submitAddModel = useCallback(async () => {
    setAddError("");
    if (!addForm.repo.trim() || !addForm.ggufFile.trim()) {
      setAddError("A repository and a .gguf file are required.");
      return;
    }
    setAddBusy(true);
    try {
      const model = {
        slug: "",
        displayName: addForm.displayName.trim() || addForm.repo.split("/").pop() || addForm.repo,
        family: addForm.family,
        repo: addForm.repo.trim(),
        ggufFile: addForm.ggufFile.trim(),
        tokenizerRepo: (addForm.tokenizerRepo.trim() || addForm.repo.trim()),
        sizeBytes: 0,
      };
      await invoke("add_enhancement_model", { model });
      setShowAddModel(false);
      setAddForm({ repo: "", ggufFile: "", family: "qwen2", tokenizerRepo: "", displayName: "" });
      setGgufChoices([]);
      await refresh();
    } catch (e: unknown) {
      setAddError(e instanceof Error ? e.message : String(e));
    } finally {
      setAddBusy(false);
    }
  }, [addForm, refresh]);

  async function handleSave() {
    if (!settings) return;
    await invoke("save_settings", { settings });
    setSaved(true);
    setTimeout(() => setSaved(false), 1800);
    refresh();
  }

  async function selectModel(name: string) {
    if (!settings) return;
    const m = models.find((x) => x.name === name);
    if (!m?.runnable || !m.downloaded) return;
    const next = { ...settings, model: name };
    setSettings(next);
    await invoke("save_settings", { settings: next });
    refresh();
  }

  async function finishOnboarding() {
    await invoke("complete_onboarding");
    if (settings) {
      const next = { ...settings, onboarding_complete: true };
      setSettings(next);
      await invoke("save_settings", { settings: next });
    }
    refresh();
  }

  async function grantAccessibility() {
    setAxBusy(true);
    try {
      // Plugin path — this is what registers the app in the Accessibility list
      await requestAccessibilityPermission();
      // Our backend also opens the pane + AX prompt
      await invoke<boolean>("request_accessibility");

      // Live-repoll the OS every 750ms until it reports trusted (or we give
      // up) instead of a single fixed-delay check — the user may take a few
      // seconds to find and toggle the switch in System Settings.
      for (let attempt = 0; attempt < 20; attempt++) {
        await new Promise((r) => setTimeout(r, 750));
        let trusted = false;
        try {
          trusted = await checkAccessibilityPermission();
        } catch {
          /* keep polling */
        }
        if (trusted) break;
      }
      await refresh();
    } finally {
      setAxBusy(false);
    }
  }

  async function grantMic() {
    try {
      await requestMicrophonePermission();
    } catch {
      /* ignore */
    }
    await invoke("open_microphone_settings");
    refresh();
  }

  // Speech (ASR) models power the Models tab; enhancement LLMs live in their
  // own section under Advanced, so keep the two lists apart.
  const asrModels = useMemo(() => models.filter((m) => m.kind !== "llm"), [models]);
  const enhanceModels = useMemo(() => models.filter((m) => m.kind === "llm"), [models]);

  const filtered = useMemo(() => {
    let list = asrModels;
    if (filter === "runnable") list = list.filter((m) => m.runnable);
    if (filter === "recommended") list = list.filter((m) => m.recommended);
    if (filter === "downloaded") list = list.filter((m) => m.downloaded);
    if (!showAllCatalog && filter === "all") {
      // show recommended + runnable first; rest behind toggle... handled below
    }
    const q = query.trim().toLowerCase();
    if (q) {
      list = list.filter(
        (m) =>
          m.displayName.toLowerCase().includes(q) ||
          m.description.toLowerCase().includes(q) ||
          m.engine.toLowerCase().includes(q) ||
          m.family.toLowerCase().includes(q) ||
          m.name.toLowerCase().includes(q),
      );
    }
    return list;
  }, [asrModels, filter, query, showAllCatalog]);

  const catalogHidden =
    filter === "all" && !showAllCatalog && !query
      ? asrModels.filter((m) => !m.runnable && !m.recommended).length
      : 0;

  const displayModels = useMemo(() => {
    if (filter === "all" && !showAllCatalog && !query) {
      return asrModels.filter((m) => m.runnable || m.recommended);
    }
    return filtered;
  }, [filter, showAllCatalog, query, asrModels, filtered]);

  if (!settings || !perms) {
    return (
      <div className="s-loading">
        <span className="s-spinner" /> Loading…
      </div>
    );
  }

  const needsOnboarding = !settings.onboarding_complete;
  const canFinish = perms.microphone;

  if (needsOnboarding) {
    return (
      <Onboarding
        perms={perms}
        onRefresh={refresh}
        onFinish={finishOnboarding}
        onGrantAx={grantAccessibility}
        onGrantMic={grantMic}
        axBusy={axBusy}
        canFinish={canFinish}
      />
    );
  }

  return (
    <div className="s-shell">
      {/* Integrated, draggable title bar strip (macOS Overlay style — the
          traffic lights float over this). Interactive controls below it sit
          past the top inset so they're never covered. */}
      <div className="s-titlebar" data-tauri-drag-region />
      <aside className="s-side">
        <div className="s-brand" data-tauri-drag-region>
          <div className="s-brand__mark">
            <Logo size={22} />
          </div>
          <div>
            <div className="s-brand__name">OpenVoice</div>
            <div className="s-brand__sub">Local voice typing</div>
          </div>
        </div>

        {(
          [
            ["models", "Models"],
            ["general", "General"],
            ["enhance", "Enhancement"],
            ["history", "History"],
            ["advanced", "Advanced"],
            ["permissions", "Permissions"],
            ["about", "About"],
          ] as const
        ).map(([id, label]) => (
          <button
            key={id}
            className={`s-nav-btn ${tab === id ? "s-nav-btn--active" : ""}`}
            onClick={() => setTab(id)}
          >
            <span className="s-nav-ic">
              <NavIcon id={id} />
            </span>
            <span className="s-nav-label">{label}</span>
            {id === "permissions" && !perms.accessibility && <span className="dot" />}
          </button>
        ))}

        <div className="s-side__footer">
          Hold {hotkeyLabel(settings.hotkey)}
          <br />
          to dictate
        </div>
      </aside>

      <main className="s-main">
        {tab === "models" && (
          <>
            <header className="s-main__header">
              <h1 className="s-main__title">Models</h1>
              <p className="s-main__desc">
                {catalogTotal} on-device models, ranked by speed and accuracy — Whisper,
                Parakeet, Moonshine, SenseVoice, and more.
              </p>
            </header>
            <div className="s-main__body">
              <StatusStrip perms={perms} settings={settings} />

              <div className="s-toolbar">
                <input
                  className="s-search"
                  placeholder="Search Parakeet, Canary, Moonshine, Whisper…"
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                />
                <span className="s-count">
                  {displayModels.length} / {asrModels.length}
                </span>
              </div>

              <div className="s-filter">
                {(
                  [
                    ["all", "All"],
                    ["runnable", "Ready to use"],
                    ["recommended", "Recommended"],
                    ["downloaded", "Downloaded"],
                  ] as const
                ).map(([id, label]) => (
                  <button
                    key={id}
                    className={`s-chip-btn ${filter === id ? "s-chip-btn--on" : ""}`}
                    onClick={() => {
                      setFilter(id);
                      if (id === "all") setShowAllCatalog(true);
                    }}
                  >
                    {label}
                  </button>
                ))}
              </div>

              {displayModels.map((m) => (
                <ModelCard
                  key={m.name}
                  model={m}
                  active={settings.model === m.name}
                  download={downloads[m.name]}
                  onSelect={() => selectModel(m.name)}
                  onDownload={() => startDownload(m.name)}
                  onCancel={cancelDownload}
                  onDelete={() => deleteModel(m.name)}
                />
              ))}

              {filter === "all" && catalogHidden > 0 && !showAllCatalog && (
                <button className="s-linkish" onClick={() => setShowAllCatalog(true)}>
                  Show {catalogHidden} more catalog models
                </button>
              )}

              <button
                className="s-btn s-btn--ghost s-btn--sm"
                onClick={() => invoke("open_models_folder")}
              >
                Open models folder
              </button>
            </div>
          </>
        )}

        {tab === "general" && (
          <>
            <header className="s-main__header">
              <h1 className="s-main__title">General</h1>
              <p className="s-main__desc">Shortcut, output, and recording style.</p>
            </header>
            <div className="s-main__body">
              <div className="s-field">
                <label className="s-label">Output</label>
                <div className="s-radio-group">
                  {[
                    {
                      value: "paste",
                      label: "Paste at cursor (recommended)",
                      desc: "Clipboard + ⌘V — most reliable (needs Accessibility)",
                    },
                    {
                      value: "type",
                      label: "Type at cursor",
                      desc: "Character-by-character injection",
                    },
                    {
                      value: "clipboard",
                      label: "Copy to clipboard only",
                      desc: "You paste with ⌘V yourself — no Accessibility needed",
                    },
                  ].map((opt) => (
                    <label
                      key={opt.value}
                      className={`s-radio ${settings.output_mode === opt.value ? "s-radio--active" : ""}`}
                    >
                      <input
                        type="radio"
                        name="output_mode"
                        checked={settings.output_mode === opt.value}
                        onChange={() => setSettings({ ...settings, output_mode: opt.value })}
                      />
                      <div>
                        <div className="s-radio__label">{opt.label}</div>
                        <div className="s-radio__desc">{opt.desc}</div>
                      </div>
                    </label>
                  ))}
                </div>
              </div>

              <div className="s-field">
                <label className="s-label">Recording</label>
                <div className="s-radio-group">
                  {[
                    {
                      value: "ptt",
                      label: "Push to talk",
                      desc: "Hold shortcut → speak → release",
                    },
                    {
                      value: "toggle",
                      label: "Toggle",
                      desc: "Press once to start, again to stop",
                    },
                  ].map((opt) => (
                    <label
                      key={opt.value}
                      className={`s-radio ${(settings.recording_mode ?? "ptt") === opt.value ? "s-radio--active" : ""}`}
                    >
                      <input
                        type="radio"
                        name="recording_mode"
                        checked={(settings.recording_mode ?? "ptt") === opt.value}
                        onChange={() => setSettings({ ...settings, recording_mode: opt.value })}
                      />
                      <div>
                        <div className="s-radio__label">{opt.label}</div>
                        <div className="s-radio__desc">{opt.desc}</div>
                      </div>
                    </label>
                  ))}
                </div>
              </div>

              <ShortcutRecorder
                value={settings.hotkey}
                onSave={(accel) => setSettings({ ...settings, hotkey: accel })}
              />

              <div className="s-field">
                <label className="s-label">Language</label>
                <select
                  className="s-select"
                  value={settings.language ?? "auto"}
                  onChange={(e) => setSettings({ ...settings, language: e.target.value })}
                >
                  {LANGUAGES.map(([code, label]) => (
                    <option key={code} value={code}>
                      {label}
                    </option>
                  ))}
                </select>
                <p className="s-help">
                  Multilingual models only — English-only models ignore this.
                </p>
              </div>

              <div className="s-field">
                <label className="s-label">Active model</label>
                <select
                  className="s-select"
                  value={settings.model}
                  onChange={(e) => setSettings({ ...settings, model: e.target.value })}
                >
                  {asrModels
                    .filter((m) => m.downloaded && m.runnable)
                    .map((m) => (
                      <option key={m.name} value={m.name}>
                        {m.displayName} ({m.size})
                      </option>
                    ))}
                  {asrModels.filter((m) => m.downloaded && m.runnable).length === 0 && (
                    <option disabled>No runnable models downloaded</option>
                  )}
                </select>
              </div>

              <div className="s-field">
                <label className="s-label">Microphone</label>
                <select
                  className="s-select"
                  value={settings.microphone ?? ""}
                  onChange={(e) => setSettings({ ...settings, microphone: e.target.value })}
                >
                  <option value="">System default</option>
                  {mics.map((name) => (
                    <option key={name} value={name}>
                      {name}
                    </option>
                  ))}
                </select>
                <p className="s-help">Device list from CoreAudio. Default is used if empty.</p>
              </div>

              <div className="s-footer-actions">
                <button className="s-btn s-btn--primary" onClick={handleSave}>
                  {saved ? "Saved" : "Save"}
                </button>
              </div>
            </div>
          </>
        )}

        {tab === "history" && (
          <>
            <header className="s-main__header">
              <h1 className="s-main__title">History</h1>
              <p className="s-main__desc">Recent transcripts from this session.</p>
            </header>
            <div className="s-main__body">
              <div className="s-card__actions">
                <button
                  className="s-btn s-btn--ghost s-btn--sm"
                  onClick={async () => {
                    await invoke("clear_history");
                    setHistory([]);
                  }}
                >
                  Clear history
                </button>
                <span className="s-count">{history.length} items</span>
              </div>
              {history.length === 0 && (
                <div className="s-empty">
                  <div className="s-empty__icon">
                    <Logo size={30} />
                  </div>
                  <div className="s-empty__title">No transcripts yet</div>
                  <p className="s-empty__desc">
                    Hold {hotkeyLabel(settings.hotkey)} and speak — your transcripts will appear
                    here.
                  </p>
                </div>
              )}
              {history.map((h) => {
                const showingRaw = showRawId === h.id && !!h.raw;
                const shown = showingRaw ? (h.raw as string) : h.text;
                return (
                  <div key={h.id} className="s-card">
                    <div className="s-card__top">
                      <div className="s-card__title" style={{ fontWeight: 500, fontSize: 13 }}>
                        {shown || "—"}
                      </div>
                      <button
                        className="s-btn s-btn--ghost s-btn--sm"
                        onClick={() => {
                          invoke("copy_text", { text: shown })
                            .then(() => {
                              setCopiedId(h.id);
                              setTimeout(() => setCopiedId((cur) => (cur === h.id ? null : cur)), 1500);
                            })
                            .catch(() => {});
                        }}
                      >
                        {copiedId === h.id ? "Copied ✓" : "Copy"}
                      </button>
                    </div>
                    <p
                      className="s-help"
                      title={new Date(h.timestamp * 1000).toLocaleString()}
                      style={{ display: "flex", gap: 10, alignItems: "center" }}
                    >
                      <span>{relativeTime(h.timestamp)}</span>
                      {h.raw && (
                        <>
                          <span style={{ opacity: 0.4 }}>·</span>
                          <button
                            className="s-linkish"
                            onClick={() => setShowRawId((cur) => (cur === h.id ? null : h.id))}
                          >
                            {showingRaw ? "Show enhanced" : "Show original"}
                          </button>
                        </>
                      )}
                      {showingRaw && <span style={{ opacity: 0.5 }}>original transcript</span>}
                    </p>
                  </div>
                );
              })}
            </div>
          </>
        )}

        {tab === "enhance" && (
          <>
            <header className="s-main__header">
              <h1 className="s-main__title">Enhancement</h1>
              <p className="s-main__desc">
                Polish your dictation with a small on-device LLM before it's pasted.
              </p>
            </header>
            <div className="s-main__body">
              <label className="s-card s-card--clickable" style={{ display: "flex", gap: 12, alignItems: "center" }}>
                <input
                  type="checkbox"
                  checked={!!settings.enhance_enabled}
                  onChange={(e) => setSettings({ ...settings, enhance_enabled: e.target.checked })}
                />
                <div>
                  <div className="s-card__title">Enhance transcripts with a local LLM</div>
                  <p className="s-card__desc" style={{ marginBottom: 0 }}>
                    Cleans up filler words and fixes punctuation before your text is pasted. If it's
                    ever slow or unavailable, your raw transcript is used — nothing is ever lost, and
                    nothing leaves your Mac.
                  </p>
                </div>
              </label>

              {settings.enhance_enabled && (
                <>
                  <div className="s-section-label">Model</div>
                  {enhanceModels.length === 0 && (
                    <p className="s-help">No enhancement models available in the catalog.</p>
                  )}
                  {enhanceModels.map((m) => {
                    const dl = downloads[m.name];
                    const isSelected = settings.enhance_model === m.name;
                    return (
                      <div className="s-card" key={m.name}>
                        <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
                          <div style={{ flex: 1, minWidth: 0 }}>
                            <div className="s-card__title">
                              {m.displayName}{" "}
                              <span style={{ opacity: 0.5, fontWeight: 400 }}>· {m.size}</span>
                            </div>
                            <p className="s-card__desc" style={{ marginBottom: 0 }}>
                              {m.description}
                            </p>
                          </div>
                          {m.downloaded ? (
                            <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                              {isSelected ? (
                                <span className="s-rank" style={{ whiteSpace: "nowrap" }}>
                                  In use
                                </span>
                              ) : (
                                <button
                                  className="s-btn"
                                  onClick={() => {
                                    const next = { ...settings, enhance_model: m.name };
                                    setSettings(next);
                                    invoke("save_settings", { settings: next }).then(refresh);
                                  }}
                                >
                                  Use
                                </button>
                              )}
                              <button
                                className="s-btn s-btn--ghost"
                                onClick={() => (m.custom ? removeCustomModel(m.name) : deleteModel(m.name))}
                              >
                                Delete
                              </button>
                            </div>
                          ) : dl?.active ? (
                            <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                              <span className="s-count">{dl.pct}%</span>
                              <button className="s-btn s-btn--ghost" onClick={cancelDownload}>
                                Cancel
                              </button>
                            </div>
                          ) : (
                            <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                              <button className="s-btn s-btn--primary" onClick={() => startDownload(m.name)}>
                                Download
                              </button>
                              {m.custom && (
                                <button
                                  className="s-btn s-btn--ghost"
                                  onClick={() => removeCustomModel(m.name)}
                                >
                                  Remove
                                </button>
                              )}
                            </div>
                          )}
                        </div>
                        {dl?.error && (
                          <p className="s-help" style={{ color: "var(--danger, #ff6b6b)" }}>
                            {dl.error}
                          </p>
                        )}
                      </div>
                    );
                  })}

                  {!showAddModel ? (
                    <button
                      className="s-btn s-btn--ghost"
                      style={{ alignSelf: "flex-start" }}
                      onClick={() => setShowAddModel(true)}
                    >
                      + Add a model from HuggingFace
                    </button>
                  ) : (
                    <div className="s-card">
                      <div className="s-card__title" style={{ marginBottom: 8 }}>
                        Add a custom GGUF model
                      </div>
                      <p className="s-card__desc">
                        Paste a HuggingFace repository that contains a GGUF file, pick the file, and
                        tell us its family so it's loaded with the right chat format. Supported
                        families: Qwen3, Qwen2.5, Llama&nbsp;3.x, Mistral. Results vary by model —
                        the built-in Qwen3 models are the tuned, known-good defaults.
                      </p>

                      <div className="s-field">
                        <label className="s-label">GGUF repository</label>
                        <div style={{ display: "flex", gap: 8 }}>
                          <input
                            className="s-input"
                            style={{ flex: 1 }}
                            placeholder="e.g. unsloth/Qwen2.5-1.5B-Instruct-GGUF"
                            value={addForm.repo}
                            onChange={(e) => setAddForm({ ...addForm, repo: e.target.value })}
                          />
                          <button className="s-btn" onClick={fetchGgufFiles} disabled={addBusy || !addForm.repo.trim()}>
                            {addBusy ? "…" : "Fetch files"}
                          </button>
                        </div>
                      </div>

                      {ggufChoices.length > 0 && (
                        <div className="s-field">
                          <label className="s-label">GGUF file (quantization)</label>
                          <select
                            className="s-select"
                            value={addForm.ggufFile}
                            onChange={(e) => setAddForm({ ...addForm, ggufFile: e.target.value })}
                          >
                            {ggufChoices.map((f) => (
                              <option key={f} value={f}>
                                {f}
                              </option>
                            ))}
                          </select>
                          <p className="s-help">Q4_K_M is a good size/quality balance for on-device use.</p>
                        </div>
                      )}

                      <div className="s-field">
                        <label className="s-label">Family (chat format)</label>
                        <select
                          className="s-select"
                          value={addForm.family}
                          onChange={(e) => setAddForm({ ...addForm, family: e.target.value })}
                        >
                          <option value="qwen3">Qwen3</option>
                          <option value="qwen2">Qwen2.5</option>
                          <option value="llama3">Llama 3.x</option>
                          <option value="mistral">Mistral</option>
                        </select>
                      </div>

                      <div className="s-field">
                        <label className="s-label">Tokenizer repository (optional)</label>
                        <input
                          className="s-input"
                          placeholder="Base model repo with tokenizer.json — e.g. Qwen/Qwen2.5-1.5B-Instruct"
                          value={addForm.tokenizerRepo}
                          onChange={(e) => setAddForm({ ...addForm, tokenizerRepo: e.target.value })}
                        />
                        <p className="s-help">
                          Where to fetch <code>tokenizer.json</code>. Leave blank to use the GGUF repo
                          itself; set the base (non-GGUF) model repo if the GGUF repo has no tokenizer.
                        </p>
                      </div>

                      <div className="s-field">
                        <label className="s-label">Display name (optional)</label>
                        <input
                          className="s-input"
                          placeholder="Shown in the list"
                          value={addForm.displayName}
                          onChange={(e) => setAddForm({ ...addForm, displayName: e.target.value })}
                        />
                      </div>

                      {addError && (
                        <p className="s-help" style={{ color: "var(--danger, #ff6b6b)" }}>
                          {addError}
                        </p>
                      )}

                      <div style={{ display: "flex", gap: 8, marginTop: 4 }}>
                        <button className="s-btn s-btn--primary" onClick={submitAddModel} disabled={addBusy}>
                          Add model
                        </button>
                        <button
                          className="s-btn s-btn--ghost"
                          onClick={() => {
                            setShowAddModel(false);
                            setAddError("");
                          }}
                        >
                          Cancel
                        </button>
                      </div>
                    </div>
                  )}

                  <div className="s-section-label">Style</div>
                  <div className="s-field">
                    <label className="s-label">Style</label>
                    <select
                      className="s-select"
                      value={settings.enhance_mode ?? "clean"}
                      onChange={(e) => setSettings({ ...settings, enhance_mode: e.target.value })}
                    >
                      <option value="auto">Automatic (per app)</option>
                      <option value="clean">Clean up (fillers, punctuation)</option>
                      <option value="message">Casual message</option>
                      <option value="email">Professional email</option>
                      <option value="notes">Bullet-point notes</option>
                      <option value="custom">Custom…</option>
                    </select>
                    <p className="s-help">
                      {settings.enhance_mode === "auto"
                        ? "The model is told which app you're dictating into and matches its tone — a casual message in Slack, a proper email in Mail, concise notes in a docs app, and so on."
                        : 'How the model reshapes your words. "Clean up" keeps your wording and just tidies it.'}
                    </p>
                  </div>

                  <div className="s-field">
                    <label className="s-label">Intensity</label>
                    <select
                      className="s-select"
                      value={settings.enhance_intensity ?? "balanced"}
                      onChange={(e) => setSettings({ ...settings, enhance_intensity: e.target.value })}
                    >
                      <option value="light">Light — tidy up, keep my words</option>
                      <option value="balanced">Medium — restructure &amp; tighten</option>
                      <option value="strong">Heavy — full rewrite</option>
                    </select>
                    <p className="s-help">
                      How much to rewrite. Light fixes grammar and keeps your wording; Medium and
                      Heavy actively reorganize and polish (and will change your text noticeably).
                    </p>
                  </div>

                  {(() => {
                    const mt = settings.enhance_max_tokens ?? 0;
                    const presets = [0, 256, 512, 1024, 2048, -1];
                    const isPreset = presets.includes(mt);
                    return (
                      <div className="s-field">
                        <label className="s-label">Maximum length</label>
                        <select
                          className="s-select"
                          value={isPreset ? String(mt) : "custom"}
                          onChange={(e) => {
                            const v = e.target.value;
                            if (v === "custom") {
                              // Seed the custom box with a sensible starting number.
                              setSettings({ ...settings, enhance_max_tokens: 1024 });
                            } else {
                              setSettings({ ...settings, enhance_max_tokens: Number(v) });
                            }
                          }}
                        >
                          <option value="0">Auto (scale to what I said)</option>
                          <option value="256">Short (~256 tokens)</option>
                          <option value="512">Medium (~512 tokens)</option>
                          <option value="1024">Long (~1024 tokens)</option>
                          <option value="2048">Very long (~2048 tokens)</option>
                          <option value="-1">Unconstrained</option>
                          <option value="custom">Custom…</option>
                        </select>
                        {!isPreset && (
                          <input
                            className="s-input"
                            type="number"
                            min={32}
                            max={4096}
                            step={64}
                            value={mt}
                            onChange={(e) =>
                              setSettings({ ...settings, enhance_max_tokens: Number(e.target.value) || 0 })
                            }
                            style={{ marginTop: 8, maxWidth: 160 }}
                          />
                        )}
                        <p className="s-help">
                          How many tokens the model may generate (roughly ¾ of a word each). "Auto"
                          fits both short cleanups and emails. Raise it if longer command-mode
                          outputs (emails, notes) get cut off. "Unconstrained" lets the model run
                          until it's done — still bounded by a time limit so it can't hang, and a
                          longer budget means a longer wait before your text appears.
                        </p>
                      </div>
                    );
                  })()}

                  <label className="s-card s-card--clickable" style={{ display: "flex", gap: 12, alignItems: "center" }}>
                    <input
                      type="checkbox"
                      checked={!!settings.enhance_voice_commands}
                      onChange={(e) =>
                        setSettings({ ...settings, enhance_voice_commands: e.target.checked })
                      }
                    />
                    <div>
                      <div className="s-card__title">Follow spoken commands</div>
                      <p className="s-card__desc" style={{ marginBottom: 0 }}>
                        If you say an instruction like "make this more professional" or "turn this
                        into bullet points," the model carries it out instead of typing it. Powerful,
                        but can occasionally act on words you meant literally.
                      </p>
                    </div>
                  </label>

                  {settings.enhance_mode === "custom" && (
                    <div className="s-field">
                      <label className="s-label">Custom instruction</label>
                      <textarea
                        className="s-input"
                        rows={3}
                        maxLength={2000}
                        placeholder="e.g. Rewrite as a concise Slack message in a friendly tone."
                        value={settings.enhance_custom_prompt ?? ""}
                        onChange={(e) =>
                          setSettings({ ...settings, enhance_custom_prompt: e.target.value })
                        }
                      />
                      <p className="s-help">
                        Applied on top of the safety rules (never adds facts, never translates,
                        outputs only your text).
                      </p>
                    </div>
                  )}

                  <div className="s-section-label">Debugging</div>
                  <label className="s-card s-card--clickable" style={{ display: "flex", gap: 12, alignItems: "center" }}>
                    <input
                      type="checkbox"
                      checked={!!settings.enhance_debug_log}
                      onChange={(e) =>
                        setSettings({ ...settings, enhance_debug_log: e.target.checked })
                      }
                    />
                    <div>
                      <div className="s-card__title">Log enhancement details to a file</div>
                      <p className="s-card__desc" style={{ marginBottom: 0 }}>
                        Records the raw transcript, the exact prompt sent to the model, and its
                        response for each dictation. Stays on your Mac — useful for tuning styles.
                      </p>
                    </div>
                  </label>
                  {settings.enhance_debug_log && (
                    <div style={{ display: "flex", gap: 8 }}>
                      <button
                        className="s-btn s-btn--ghost s-btn--sm"
                        onClick={() => invoke("open_enhancement_log").catch(() => {})}
                      >
                        Open log
                      </button>
                      <button
                        className="s-btn s-btn--ghost s-btn--sm"
                        onClick={() => invoke("clear_enhancement_log").catch(() => {})}
                      >
                        Clear log
                      </button>
                    </div>
                  )}

                  <div style={{ marginTop: 8 }}>
                    <button className="s-btn s-btn--primary" onClick={handleSave}>
                      {saved ? "Saved ✓" : "Save"}
                    </button>
                  </div>
                </>
              )}
            </div>
          </>
        )}

        {tab === "advanced" && (
          <>
            <header className="s-main__header">
              <h1 className="s-main__title">Advanced</h1>
              <p className="s-main__desc">Fine-tune transcription, overlay, and system behaviour.</p>
            </header>
            <div className="s-main__body">
              <div className="s-section-label">Transcription</div>

              <label className="s-card s-card--clickable" style={{ display: "flex", gap: 12, alignItems: "center" }}>
                <input
                  type="checkbox"
                  checked={!!settings.append_trailing_space}
                  onChange={(e) =>
                    setSettings({ ...settings, append_trailing_space: e.target.checked })
                  }
                />
                <div>
                  <div className="s-card__title">Append trailing space</div>
                  <p className="s-card__desc" style={{ marginBottom: 0 }}>
                    Add a space after each transcript so the next word is separated.
                  </p>
                </div>
              </label>

              <label className="s-card s-card--clickable" style={{ display: "flex", gap: 12, alignItems: "center" }}>
                <input
                  type="checkbox"
                  checked={!!settings.audio_feedback}
                  onChange={(e) => setSettings({ ...settings, audio_feedback: e.target.checked })}
                />
                <div>
                  <div className="s-card__title">Audio feedback</div>
                  <p className="s-card__desc" style={{ marginBottom: 0 }}>
                    Play a soft system sound when recording starts and stops.
                  </p>
                </div>
              </label>

              <div className="s-field">
                <label className="s-label">Mic sensitivity</label>
                <input
                  className="s-input"
                  type="range"
                  min={0.001}
                  max={0.03}
                  step={0.001}
                  value={settings.silence_threshold ?? 0.005}
                  onChange={(e) =>
                    setSettings({ ...settings, silence_threshold: Number(e.target.value) })
                  }
                />
                <p className="s-help">
                  Lower = picks up quieter speech (may also catch background noise).
                </p>
              </div>

              <div className="s-section-label">Overlay</div>

              <label className="s-card s-card--clickable" style={{ display: "flex", gap: 12, alignItems: "center" }}>
                <input
                  type="checkbox"
                  checked={!!settings.show_overlay}
                  onChange={(e) => setSettings({ ...settings, show_overlay: e.target.checked })}
                />
                <div>
                  <div className="s-card__title">Show recording overlay</div>
                  <p className="s-card__desc" style={{ marginBottom: 0 }}>
                    Floating pill while recording and transcribing.
                  </p>
                </div>
              </label>

              <label className="s-card s-card--clickable" style={{ display: "flex", gap: 12, alignItems: "center" }}>
                <input
                  type="checkbox"
                  checked={!!settings.live_preview}
                  onChange={(e) => setSettings({ ...settings, live_preview: e.target.checked })}
                />
                <div>
                  <div className="s-card__title">Live preview while recording</div>
                  <p className="s-card__desc" style={{ marginBottom: 0 }}>
                    Gradually show a rough transcript in the overlay as you speak, instead of
                    only after you stop. Uses extra CPU/GPU during recording.
                  </p>
                </div>
              </label>

              <label className="s-card s-card--clickable" style={{ display: "flex", gap: 12, alignItems: "center" }}>
                <input
                  type="checkbox"
                  checked={!!settings.show_transcript_in_overlay}
                  onChange={(e) =>
                    setSettings({ ...settings, show_transcript_in_overlay: e.target.checked })
                  }
                />
                <div>
                  <div className="s-card__title">Show transcript in overlay</div>
                  <p className="s-card__desc" style={{ marginBottom: 0 }}>
                    After transcribing, briefly echo the finished text in the overlay before it
                    hides. Off by default — the text is already inserted where you're typing.
                  </p>
                </div>
              </label>

              <div className="s-section-label">System</div>

              <label className="s-card s-card--clickable" style={{ display: "flex", gap: 12, alignItems: "center" }}>
                <input
                  type="checkbox"
                  checked={launchAtLogin}
                  onChange={(e) => toggleLaunchAtLogin(e.target.checked)}
                />
                <div>
                  <div className="s-card__title">Launch at login</div>
                  <p className="s-card__desc" style={{ marginBottom: 0 }}>
                    Start OpenVoice automatically when you log in.
                  </p>
                </div>
              </label>

              <label className="s-card s-card--clickable" style={{ display: "flex", gap: 12, alignItems: "center" }}>
                <input
                  type="checkbox"
                  checked={!!settings.start_hidden}
                  onChange={(e) => setSettings({ ...settings, start_hidden: e.target.checked })}
                />
                <div>
                  <div className="s-card__title">Start hidden</div>
                  <p className="s-card__desc" style={{ marginBottom: 0 }}>
                    Don’t open Settings on launch (tray only).
                  </p>
                </div>
              </label>

              <label className="s-card s-card--clickable" style={{ display: "flex", gap: 12, alignItems: "center" }}>
                <input
                  type="checkbox"
                  checked={!!settings.hide_dock_icon}
                  onChange={(e) => setSettings({ ...settings, hide_dock_icon: e.target.checked })}
                />
                <div>
                  <div className="s-card__title">Hide Dock icon</div>
                  <p className="s-card__desc" style={{ marginBottom: 0 }}>
                    Never show in the Dock, even while this Settings window is open. Still
                    reachable from the menu bar tray icon.
                  </p>
                </div>
              </label>

              <div className="s-section-label">Appearance</div>

              <div className="s-field">
                <label className="s-label">Theme</label>
                <select
                  className="s-select"
                  value={settings.theme ?? "system"}
                  onChange={(e) => setSettings({ ...settings, theme: e.target.value })}
                >
                  <option value="system">Match system</option>
                  <option value="light">Light</option>
                  <option value="dark">Dark</option>
                </select>
              </div>

              <div className="s-section-label">Data</div>

              <div className="s-field">
                <label className="s-label">History limit</label>
                <input
                  className="s-input"
                  type="number"
                  min={1}
                  max={500}
                  value={settings.history_limit ?? 50}
                  onChange={(e) =>
                    setSettings({
                      ...settings,
                      history_limit: Math.max(1, Number(e.target.value) || 50),
                    })
                  }
                />
              </div>

              <div className="s-field">
                <label className="s-label">Settings backup</label>
                <div style={{ display: "flex", gap: 8 }}>
                  <button className="s-btn s-btn--ghost s-btn--sm" onClick={exportSettings}>
                    Export…
                  </button>
                  <button className="s-btn s-btn--ghost s-btn--sm" onClick={importSettings}>
                    Import…
                  </button>
                </div>
                {importError && (
                  <p className="s-help" style={{ color: "var(--danger)" }}>
                    {importError}
                  </p>
                )}
              </div>

              <div className="s-footer-actions">
                <button className="s-btn s-btn--primary" onClick={handleSave}>
                  {saved ? "Saved" : "Save"}
                </button>
              </div>
            </div>
          </>
        )}

        {tab === "about" && (
          <>
            <header className="s-main__header">
              <h1 className="s-main__title">About</h1>
            </header>
            <div className="s-main__body">
              <div className="s-about">
                <div className="s-about__mark">
                  <Logo size={44} />
                </div>
                <div className="s-about__name">OpenVoice</div>
                <div className="s-about__version">Version 0.1.0</div>
                <p className="s-about__tag">
                  Local-first voice typing for macOS. Your audio never leaves this Mac —
                  transcription runs entirely on-device.
                </p>
                <div className="s-meta" style={{ justifyContent: "center", marginBottom: 0 }}>
                  <span className="s-pill">{models.length} models</span>
                  <span className="s-pill">
                    {models.filter((m) => m.downloaded).length} downloaded
                  </span>
                  <span className="s-pill">100% offline</span>
                </div>
              </div>
              <div className="s-card">
                <div className="s-card__title">Data location</div>
                <p className="s-card__desc">
                  Models, settings, and history are stored locally in your Library folder.
                </p>
                <div className="s-path">
                  ~/Library/Application Support/com.openvoice.app/
                </div>
                <div className="s-card__actions" style={{ marginTop: 12 }}>
                  <button
                    className="s-btn s-btn--ghost s-btn--sm"
                    onClick={() => invoke("open_models_folder")}
                  >
                    Open models folder
                  </button>
                </div>
              </div>
            </div>
          </>
        )}

        {tab === "permissions" && (
          <>
            <header className="s-main__header">
              <h1 className="s-main__title">Permissions</h1>
              <p className="s-main__desc">
                {perms.is_dev ? (
                  <>
                    You’re on <strong>tauri dev</strong> — look for{" "}
                    <span className="s-code">{perms.process_name}</span>, not “OpenVoice”.
                  </>
                ) : (
                  <>
                    Enable Accessibility for <strong>{perms.process_name}</strong> so typing works.
                  </>
                )}
              </p>
            </header>
            <div className="s-main__body">
              <StatusStrip perms={perms} settings={settings} />

              {!perms.accessibility && (
                <div className="s-card s-card--featured">
                  <div className="s-card__title">If you already added the binary</div>
                  <p className="s-card__desc" style={{ marginBottom: 10 }}>
                    Adding it to the list is not enough — the switch next to{" "}
                    <span className="s-code">{perms.process_name}</span> must be{" "}
                    <strong>green / ON</strong>, then you must <strong>fully quit</strong> this app
                    and start it again. macOS does not apply Accessibility until the next launch.
                  </p>
                  <ol className="s-howto">
                    <li>
                      <span className="s-howto__n">1</span>
                      <span>
                        In Accessibility, confirm the toggle for{" "}
                        <span className="s-code">{perms.process_name}</span> is <strong>ON</strong>.
                        If it was already on, turn it <strong>off</strong>, wait 1s, turn it{" "}
                        <strong>on</strong> again.
                      </span>
                    </li>
                    <li>
                      <span className="s-howto__n">2</span>
                      <span>
                        Click <strong>Quit OpenVoice</strong> below (or menu bar → Quit). Do not just
                        close the window.
                      </span>
                    </li>
                    <li>
                      <span className="s-howto__n">3</span>
                      <span>
                        Run <span className="s-code">npm run tauri dev</span> again. Status should
                        flip to On. Rebuilding can reset the grant — you may need to re-toggle after
                        cargo rebuilds the binary.
                      </span>
                    </li>
                  </ol>
                  <div className="s-card__actions" style={{ marginTop: 12 }}>
                    <button
                      className="s-btn s-btn--accent"
                      onClick={() => invoke("quit_app")}
                    >
                      Quit OpenVoice
                    </button>
                    <button className="s-btn s-btn--ghost" onClick={refresh}>
                      Recheck now
                    </button>
                  </div>
                </div>
              )}

              <div className="s-card s-card--featured">
                <div className="s-card__top">
                  <div className="s-card__title">Accessibility</div>
                  <span className={`s-badge ${perms.accessibility ? "s-badge--on" : "s-badge--accent"}`}>
                    {perms.accessibility ? "On" : "Off"}
                  </span>
                </div>
                <p className="s-card__desc">
                  Required to type into other apps. Without it, text is still transcribed and copied
                  to the clipboard.
                </p>

                {perms.is_dev && (
                  <p className="s-card__desc">
                    Dev mode name: <span className="s-code">{perms.process_name}</span> (not
                    “OpenVoice”).
                  </p>
                )}

                <ol className="s-howto">
                  <li>
                    <span className="s-howto__n">1</span>
                    <span>
                      Click <strong>Enable Accessibility</strong> (system prompt + opens Settings).
                    </span>
                  </li>
                  <li>
                    <span className="s-howto__n">2</span>
                    <span>
                      Find <span className="s-code">{perms.process_name}</span> and turn the switch{" "}
                      <strong>ON</strong>.
                    </span>
                  </li>
                  <li>
                    <span className="s-howto__n">3</span>
                    <span>
                      Missing from the list? <strong>Reveal in Finder</strong> → Accessibility →{" "}
                      <strong>+</strong> → pick that exact file.
                    </span>
                  </li>
                  <li>
                    <span className="s-howto__n">4</span>
                    <span>
                      <strong>Quit OpenVoice</strong>, then start it again. The “On” badge only
                      updates after relaunch.
                    </span>
                  </li>
                </ol>

                <div className="s-path">{perms.executable_path}</div>

                <div className="s-card__actions" style={{ marginTop: 14 }}>
                  <button
                    className="s-btn s-btn--accent"
                    onClick={grantAccessibility}
                    disabled={axBusy || perms.accessibility}
                  >
                    {perms.accessibility
                      ? "Already enabled"
                      : axBusy
                        ? "Requesting…"
                        : "Enable Accessibility"}
                  </button>
                  <button
                    className="s-btn s-btn--ghost"
                    onClick={() => invoke("reveal_executable")}
                  >
                    Reveal in Finder
                  </button>
                  <button
                    className="s-btn s-btn--ghost"
                    onClick={() => invoke("copy_executable_path")}
                  >
                    Copy path
                  </button>
                  <button
                    className="s-btn s-btn--ghost"
                    onClick={() => invoke("open_accessibility_settings")}
                  >
                    Open Settings
                  </button>
                  {!perms.accessibility && (
                    <button className="s-btn s-btn--ghost" onClick={() => invoke("quit_app")}>
                      Quit OpenVoice
                    </button>
                  )}
                  <button className="s-btn s-btn--link" onClick={refresh}>
                    Recheck
                  </button>
                </div>
              </div>

              <div className="s-card">
                <div className="s-card__top">
                  <div className="s-card__title">Microphone</div>
                  <span className={`s-badge ${perms.microphone ? "s-badge--on" : ""}`}>
                    {perms.microphone ? "Ready" : "Needed"}
                  </span>
                </div>
                <p className="s-card__desc">
                  Used to capture speech. macOS prompts on first record.
                </p>
                <button className="s-btn s-btn--ghost s-btn--sm" onClick={grantMic}>
                  Request / open Microphone settings
                </button>
              </div>
            </div>
          </>
        )}
      </main>
    </div>
  );
}

function NavIcon({ id }: { id: Tab }) {
  const common = {
    width: 17,
    height: 17,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.9,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };
  switch (id) {
    case "models":
      return (
        <svg {...common}>
          <line x1="4" y1="12" x2="4" y2="12" />
          <line x1="8" y1="8" x2="8" y2="16" />
          <line x1="12" y1="4" x2="12" y2="20" />
          <line x1="16" y1="8" x2="16" y2="16" />
          <line x1="20" y1="11" x2="20" y2="13" />
        </svg>
      );
    case "general":
      return (
        <svg {...common}>
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
        </svg>
      );
    case "enhance":
      return (
        <svg {...common}>
          <path d="M12 3l2 5 5 2-5 2-2 5-2-5-5-2 5-2 2-5z" />
          <path d="M18 15l.9 2.1L21 18l-2.1.9L18 21l-.9-2.1L15 18l2.1-.9L18 15z" />
        </svg>
      );
    case "history":
      return (
        <svg {...common}>
          <circle cx="12" cy="12" r="9" />
          <polyline points="12 7 12 12 15 14" />
        </svg>
      );
    case "advanced":
      return (
        <svg {...common}>
          <line x1="4" y1="8" x2="20" y2="8" />
          <line x1="4" y1="16" x2="20" y2="16" />
          <circle cx="9" cy="8" r="2" fill="currentColor" />
          <circle cx="15" cy="16" r="2" fill="currentColor" />
        </svg>
      );
    case "permissions":
      return (
        <svg {...common}>
          <rect x="5" y="11" width="14" height="10" rx="2" />
          <path d="M8 11V7a4 4 0 0 1 8 0v4" />
        </svg>
      );
    case "about":
      return (
        <svg {...common}>
          <circle cx="12" cy="12" r="9" />
          <line x1="12" y1="11" x2="12" y2="16" />
          <line x1="12" y1="8" x2="12.01" y2="8" />
        </svg>
      );
  }
}

/** Attention-only status chips: native macOS apps surface problems, not
 *  confirmations. Renders nothing when everything is fine. */
function StatusStrip({
  perms,
  settings,
}: {
  perms: PermissionsStatus;
  settings: AppSettings;
}) {
  const issues: { label: string; kind: "bad" | "warn" }[] = [];
  if (!perms.model_ready) issues.push({ label: "Download a model to start", kind: "bad" });
  if (!perms.microphone) issues.push({ label: "Microphone access needed", kind: "warn" });
  if (!perms.accessibility && settings.output_mode !== "clipboard")
    issues.push({ label: "Accessibility needed to insert text", kind: "warn" });
  if (issues.length === 0) return null;
  return (
    <div className="s-status">
      {issues.map((i) => (
        <span key={i.label} className={`s-chip s-chip--${i.kind}`}>
          {i.label}
        </span>
      ))}
    </div>
  );
}

function ModelCard({
  model,
  active,
  download,
  onSelect,
  onDownload,
  onCancel,
  onDelete,
}: {
  model: ModelInfo;
  active: boolean;
  download?: DownloadState;
  onSelect: () => void;
  onDownload: () => void;
  onCancel: () => void;
  onDelete: () => void;
}) {
  const isDownloading = !!download?.active;
  const hasError = download?.error && !download.active;
  const canClick = model.runnable && model.downloaded && !isDownloading;

  return (
    <div
      className={[
        "s-card",
        active && model.downloaded && model.runnable ? "s-card--active" : "",
        model.recommended && !active ? "s-card--featured" : "",
        canClick ? "s-card--clickable" : "",
        !model.runnable ? "s-card--muted" : "",
      ]
        .filter(Boolean)
        .join(" ")}
      onClick={() => canClick && onSelect()}
    >
      <div className="s-card__top">
        <div className="s-card__title">
          {model.rank <= 10 && <span className="s-rank">#{model.rank}</span>}
          {model.displayName}
        </div>
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap", justifyContent: "flex-end" }}>
          {active && model.runnable ? (
            <span className="s-badge s-badge--on">Active</span>
          ) : (
            model.recommended && <span className="s-badge s-badge--accent">Recommended</span>
          )}
          {!model.runnable && <span className="s-badge">Unavailable</span>}
        </div>
      </div>
      <p className="s-card__desc">{model.description}</p>
      <div className="s-meta">
        <span className="s-pill">{model.size}</span>
        <span className="s-pill">{model.engine}</span>
        {model.speed !== "—" && <span className="s-pill">{model.speed}</span>}
        {model.languages?.[0] && <span className="s-pill">{model.languages[0]}</span>}
      </div>

      <div className="s-card__actions" onClick={(e) => e.stopPropagation()}>
        {model.downloaded && model.runnable ? (
          <>
            <span className="s-ready">{active ? "In use" : "Click to select"}</span>
            <button className="s-btn s-btn--danger s-btn--sm" onClick={onDelete}>
              Remove
            </button>
          </>
        ) : isDownloading && download ? (
          <div className="s-progress">
            <div className="s-progress__bar">
              <div className="s-progress__fill" style={{ width: `${download.pct}%` }} />
            </div>
            <div className="s-progress__meta">
              <span>
                {formatBytes(download.bytesReceived)} /{" "}
                {download.totalBytes > 0 ? formatBytes(download.totalBytes) : "?"}
              </span>
              <button className="s-btn s-btn--ghost s-btn--sm" onClick={onCancel}>
                Cancel
              </button>
            </div>
          </div>
        ) : hasError ? (
          <>
            <span className="s-error">{download?.error}</span>
            <button className="s-btn s-btn--primary s-btn--sm" onClick={onDownload}>
              Retry
            </button>
          </>
        ) : !model.runnable ? (
          <span className="s-help" style={{ marginBottom: 0 }}>
            Not selectable — known engine issue with this model.
          </span>
        ) : (
          <button className="s-btn s-btn--primary" onClick={onDownload}>
            Download {model.size}
          </button>
        )}
      </div>
    </div>
  );
}

function Onboarding({
  perms,
  onRefresh,
  onFinish,
  onGrantAx,
  onGrantMic,
  axBusy,
  canFinish,
}: {
  perms: PermissionsStatus;
  onRefresh: () => void;
  onFinish: () => void;
  onGrantAx: () => void;
  onGrantMic: () => void;
  axBusy: boolean;
  canFinish: boolean;
}) {
  return (
    <div className="ob">
      <div className="s-titlebar" data-tauri-drag-region />
      <div className="ob__inner">
        <div className="ob__mark">
          <Logo size={36} />
        </div>
        <h1 className="ob__title">Speak. It types.</h1>
        <p className="ob__sub">
          Private voice typing on your Mac. Grant access, then pick a model in Settings.
        </p>
        {perms.is_dev && (
          <p className="ob__hint" style={{ marginBottom: 16 }}>
            Dev mode: Accessibility lists <span className="s-code">{perms.process_name}</span>, not
            “OpenVoice”.
          </p>
        )}

        <div className="ob__steps">
          <div className={`ob__step ${perms.microphone ? "ob__step--done" : ""}`}>
            <div className="ob__step-icon">{perms.microphone ? "✓" : "1"}</div>
            <div className="ob__step-body">
              <div className="ob__step-title">Microphone</div>
              <div className="ob__step-desc">
                {perms.microphone ? "Input device ready" : "Allow when macOS asks"}
              </div>
            </div>
            {!perms.microphone && (
              <button className="s-btn s-btn--ghost s-btn--sm" onClick={onGrantMic}>
                Enable
              </button>
            )}
          </div>

          <div className={`ob__step ${perms.accessibility ? "ob__step--done" : ""}`}>
            <div className="ob__step-icon">{perms.accessibility ? "✓" : "2"}</div>
            <div className="ob__step-body">
              <div className="ob__step-title">Accessibility</div>
              <div className="ob__step-desc">
                {perms.accessibility
                  ? "Can type into other apps"
                  : `Enable “${perms.process_name}” in System Settings`}
              </div>
            </div>
            {!perms.accessibility && (
              <button
                className="s-btn s-btn--accent s-btn--sm"
                onClick={onGrantAx}
                disabled={axBusy}
              >
                {axBusy ? "…" : "Enable"}
              </button>
            )}
          </div>

        </div>

        {!perms.accessibility && (
          <p className="ob__hint" style={{ textAlign: "left", marginBottom: 12 }}>
            After clicking Enable, open Accessibility and turn on{" "}
            <span className="s-code">{perms.process_name}</span>. Quit &amp; reopen if status stays
            off.
          </p>
        )}

        <div className="ob__actions" style={{ marginTop: 16 }}>
          <button
            className="s-btn s-btn--primary"
            disabled={!canFinish}
            onClick={onFinish}
            style={{ padding: "11px 18px", fontSize: 14 }}
          >
            {canFinish ? "Continue" : "Allow microphone access to continue"}
          </button>
          <button className="s-linkish" style={{ alignSelf: "center" }} onClick={onRefresh}>
            Refresh status
          </button>
        </div>
      </div>
    </div>
  );
}
