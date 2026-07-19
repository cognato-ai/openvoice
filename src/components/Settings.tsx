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

type Tab = "models" | "general" | "history" | "advanced" | "permissions" | "about";
type ModelFilter = "all" | "runnable" | "recommended" | "downloaded";

function formatBytes(b: number) {
  if (b === 0) return "0 B";
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  if (b < 1024 * 1024 * 1024) return `${(b / (1024 * 1024)).toFixed(1)} MB`;
  return `${(b / (1024 * 1024 * 1024)).toFixed(2)} GB`;
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
        if (settings && model?.runnable) {
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

  const filtered = useMemo(() => {
    let list = models;
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
  }, [models, filter, query, showAllCatalog]);

  const catalogHidden =
    filter === "all" && !showAllCatalog && !query
      ? models.filter((m) => !m.runnable && !m.recommended).length
      : 0;

  const displayModels = useMemo(() => {
    if (filter === "all" && !showAllCatalog && !query) {
      return models.filter((m) => m.runnable || m.recommended);
    }
    return filtered;
  }, [filter, showAllCatalog, query, models, filtered]);

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
      <aside className="s-side">
        <div className="s-brand">
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
            ["models", "Models", "#0a84ff"],
            ["general", "General", "#8e8e93"],
            ["history", "History", "#ff9f0a"],
            ["advanced", "Advanced", "#af52de"],
            ["permissions", "Permissions", "#30d158"],
            ["about", "About", "#0a84ff"],
          ] as const
        ).map(([id, label, color]) => (
          <button
            key={id}
            className={`s-nav-btn ${tab === id ? "s-nav-btn--active" : ""}`}
            onClick={() => setTab(id)}
          >
            <span className="s-nav-ic" style={{ background: color }}>
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
                {catalogTotal} models, all runnable — ranked by speed + accuracy. Whisper,
                Parakeet, Canary, Moonshine, SenseVoice, GigaAM, and more.
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
                  {displayModels.length} / {models.length}
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
                  {models
                    .filter((m) => m.downloaded && m.runnable)
                    .map((m) => (
                      <option key={m.name} value={m.name}>
                        {m.displayName} ({m.size})
                      </option>
                    ))}
                  {models.filter((m) => m.downloaded && m.runnable).length === 0 && (
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
                <div className="s-card">
                  <p className="s-card__desc" style={{ marginBottom: 0 }}>
                    No transcripts yet. Hold your shortcut and speak.
                  </p>
                </div>
              )}
              {history.map((h) => (
                <div key={h.id} className="s-card">
                  <div className="s-card__top">
                    <div className="s-card__title" style={{ fontWeight: 500, fontSize: 13 }}>
                      {h.text || "—"}
                    </div>
                    <button
                      className="s-btn s-btn--ghost s-btn--sm"
                      onClick={() => invoke("copy_executable_path").catch(() => {})}
                      // copy transcript
                      onMouseDown={(e) => {
                        e.preventDefault();
                        navigator.clipboard?.writeText(h.text);
                      }}
                    >
                      Copy
                    </button>
                  </div>
                  <p className="s-help">
                    {new Date(h.timestamp * 1000).toLocaleString()}
                  </p>
                </div>
              ))}
            </div>
          </>
        )}

        {tab === "advanced" && (
          <>
            <header className="s-main__header">
              <h1 className="s-main__title">Advanced</h1>
              <p className="s-main__desc">Behaviour tweaks similar to Handy’s advanced panel.</p>
            </header>
            <div className="s-main__body">
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
                  checked={!!settings.show_overlay}
                  onChange={(e) => setSettings({ ...settings, show_overlay: e.target.checked })}
                />
                <div>
                  <div className="s-card__title">Show recording overlay</div>
                  <p className="s-card__desc" style={{ marginBottom: 0 }}>
                    Floating HUD while recording / transcribing.
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
                  checked={!!settings.audio_feedback}
                  onChange={(e) => setSettings({ ...settings, audio_feedback: e.target.checked })}
                />
                <div>
                  <div className="s-card__title">Audio feedback</div>
                  <p className="s-card__desc" style={{ marginBottom: 0 }}>
                    Play a sound when recording starts/stops (coming soon if enabled).
                  </p>
                </div>
              </label>

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
              <p className="s-main__desc">OpenVoice — local-first voice typing for macOS.</p>
            </header>
            <div className="s-main__body">
              <div className="s-card">
                <div className="s-card__title">Version 0.1.0</div>
                <p className="s-card__desc">
                  Privacy-first speech-to-text. Audio stays on your Mac. Models: Whisper (runnable)
                  + Handy catalog for browse/download.
                </p>
                <div className="s-meta">
                  <span className="s-pill">{models.length} models listed</span>
                  <span className="s-pill">
                    {models.filter((m) => m.runnable).length} ready to use
                  </span>
                  <span className="s-pill">
                    {models.filter((m) => m.downloaded).length} downloaded
                  </span>
                </div>
              </div>
              <div className="s-card">
                <div className="s-card__title">Data location</div>
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
              <div className="s-card">
                <div className="s-card__title">Disk note</div>
                <p className="s-card__desc" style={{ marginBottom: 0 }}>
                  `npm run tauri build` creates multi‑GB Rust artifacts under{" "}
                  <span className="s-code">src-tauri/target</span>. Run{" "}
                  <span className="s-code">npm run clean</span> to reclaim space (keeps your source).
                </p>
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
    width: 13,
    height: 13,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 2.2,
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

function StatusStrip({
  perms,
  settings,
}: {
  perms: PermissionsStatus;
  settings: AppSettings;
}) {
  return (
    <div className="s-status">
      <span className={`s-chip ${perms.model_ready ? "s-chip--ok" : "s-chip--bad"}`}>
        {perms.model_ready ? "Model ready" : "Need a model"}
      </span>
      <span className={`s-chip ${perms.microphone ? "s-chip--ok" : "s-chip--warn"}`}>
        {perms.microphone ? "Mic OK" : "Mic missing"}
      </span>
      <span className={`s-chip ${perms.accessibility ? "s-chip--ok" : "s-chip--warn"}`}>
        {perms.accessibility
          ? "Accessibility on"
          : settings.output_mode === "type"
            ? "Accessibility off"
            : "Accessibility optional"}
      </span>
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
          {model.rank <= 10 && <span className="s-badge" style={{ marginRight: 6 }}>#{model.rank}</span>}
          {model.displayName}
        </div>
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap", justifyContent: "flex-end" }}>
          {model.recommended && <span className="s-badge s-badge--accent">Recommended</span>}
          {active && model.runnable && <span className="s-badge s-badge--on">Active</span>}
          {model.runnable ? (
            <span className="s-badge">Runnable</span>
          ) : (
            <span className="s-badge">Catalog</span>
          )}
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
            <span className="s-ready">Ready{active ? " · in use" : " · click to use"}</span>
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
        ) : model.downloaded && !model.runnable ? (
          <span className="s-ready">Downloaded (engine later)</span>
        ) : (
          <button
            className={`s-btn ${model.runnable ? "s-btn--primary" : "s-btn--ghost"}`}
            onClick={onDownload}
          >
            Download {model.size}
            {!model.runnable ? " · preview" : ""}
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
