import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow, LogicalPosition, LogicalSize } from "@tauri-apps/api/window";
import "./HUD.css";

type RecordingState = "idle" | "recording" | "transcribing" | "done" | "error";

interface AppSettings {
  model: string;
  output_mode: string;
  hotkey: string;
  recording_mode: string;
  live_preview?: boolean;
}

const HUD_W = 280;
const HUD_H = 48;
const HUD_H_PREVIEW = 80;

async function positionHUD(height = HUD_H) {
  try {
    const win = getCurrentWindow();
    const screenW = window.screen.width;
    const screenH = window.screen.height;
    const x = Math.round((screenW - HUD_W) / 2);
    const y = screenH - 130 - (height - HUD_H);
    await win.setSize(new LogicalSize(HUD_W, height));
    await win.setPosition(new LogicalPosition(x, y));
  } catch {
    /* ignore */
  }
}

export default function HUD() {
  const [state, setState] = useState<RecordingState>("idle");
  const WAVE_BARS = 7;
  const [levels, setLevels] = useState<number[]>(Array(WAVE_BARS).fill(0));
  const [lastText, setLastText] = useState("");
  const [elapsedSecs, setElapsedSecs] = useState(0);
  const [previewText, setPreviewText] = useState("");

  const levelTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  const recordingTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  const previewTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  const isRecordingRef = useRef(false);
  const settingsRef = useRef<AppSettings>({
    model: "whisper-tiny.en",
    output_mode: "type",
    hotkey: "Alt+Space",
    recording_mode: "ptt",
    live_preview: false,
  });

  useEffect(() => {
    positionHUD();
    invoke<AppSettings>("get_settings").then((s) => {
      settingsRef.current = s;
    });
  }, []);

  const showHUD = useCallback(async () => {
    try {
      // Respect show_overlay setting
      try {
        const s = await invoke<{ show_overlay?: boolean }>("get_settings");
        if (s.show_overlay === false) return;
      } catch {
        /* show by default */
      }
      await positionHUD();
      await getCurrentWindow().show();
    } catch {
      /* ignore */
    }
  }, []);

  const hideHUD = useCallback(async () => {
    try {
      await getCurrentWindow().hide();
    } catch {
      /* ignore */
    }
  }, []);

  const startRecording = useCallback(async () => {
    if (isRecordingRef.current) return;
    try {
      settingsRef.current = await invoke<AppSettings>("get_settings");
      await showHUD();
      await invoke("start_recording");
      isRecordingRef.current = true;
      setState("recording");
      setElapsedSecs(0);
      setPreviewText("");
    } catch (e: unknown) {
      const msg = typeof e === "string" ? e : "Failed to start recording";
      setLastText(msg);
      setState("error");
      await showHUD();
      setTimeout(() => {
        setState("idle");
        hideHUD();
      }, 3500);
    }
  }, [showHUD, hideHUD]);

  const stopRecording = useCallback(async () => {
    if (!isRecordingRef.current) return;
    isRecordingRef.current = false;
    setState("transcribing");
    try {
      // A hang anywhere in the backend chain (a slow first-time model load,
      // a stuck channel, anything) must never leave the overlay stuck open
      // forever — bound it and surface a timeout as an error instead.
      const text = await Promise.race([
        invoke<string>("stop_recording_and_transcribe"),
        new Promise<string>((_, reject) =>
          setTimeout(() => reject("Timed out waiting for transcription"), 20000),
        ),
      ]);
      if (text && text.trim().length > 0) {
        setLastText(text.trim());
        setState("done");
        setTimeout(() => {
          setState("idle");
          hideHUD();
        }, 2200);
      } else {
        setLastText("No speech detected");
        setState("error");
        setTimeout(() => {
          setState("idle");
          hideHUD();
        }, 1800);
      }
    } catch (e: unknown) {
      const msg = typeof e === "string" ? e : "Transcription failed";
      setLastText(msg);
      setState("error");
      setTimeout(() => {
        setState("idle");
        hideHUD();
      }, 4000);
    }
  }, [hideHUD]);

  useEffect(() => {
    const unlistenPressed = listen("shortcut-pressed", async () => {
      const mode = settingsRef.current.recording_mode;
      if (mode === "toggle") {
        if (isRecordingRef.current) await stopRecording();
        else await startRecording();
      } else {
        await startRecording();
      }
    });

    const unlistenReleased = listen("shortcut-released", async () => {
      if (settingsRef.current.recording_mode === "ptt" && isRecordingRef.current) {
        await stopRecording();
      }
    });

    return () => {
      unlistenPressed.then((f) => f());
      unlistenReleased.then((f) => f());
    };
  }, [startRecording, stopRecording]);

  useEffect(() => {
    // Only ever touch window geometry while actively recording and visible.
    // Calling setSize/setPosition once hidden risks re-ordering a hidden
    // window back onto screen on macOS — the reset back to HUD_H happens in
    // showHUD() at the start of the *next* recording instead.
    if (state === "recording") {
      positionHUD(previewText ? HUD_H_PREVIEW : HUD_H);
    }
  }, [state, previewText]);

  useEffect(() => {
    if (state === "recording") {
      levelTimer.current = setInterval(async () => {
        const lvl = await invoke<number>("get_audio_level");
        const sample = Math.min(lvl * 12, 1);
        // Roll the newest sample in and shift the rest along, so each bar
        // reflects a genuinely different recent moment (real amplitude
        // variation across the waveform) instead of one shared value
        // differentiated only by a fake CSS animation-delay offset.
        setLevels((prev) => [...prev.slice(1), sample]);
      }, 90);
      recordingTimer.current = setInterval(() => {
        setElapsedSecs((s) => s + 1);
      }, 1000);
      if (settingsRef.current.live_preview) {
        previewTimer.current = setInterval(async () => {
          try {
            const text = await invoke<string>("get_partial_transcript");
            if (text) setPreviewText(text);
          } catch {
            /* ignore — try again next tick */
          }
        }, 1800);
      }
    } else {
      if (levelTimer.current) clearInterval(levelTimer.current);
      if (recordingTimer.current) clearInterval(recordingTimer.current);
      if (previewTimer.current) clearInterval(previewTimer.current);
      setLevels(Array(WAVE_BARS).fill(0));
    }
    return () => {
      if (levelTimer.current) clearInterval(levelTimer.current);
      if (recordingTimer.current) clearInterval(recordingTimer.current);
      if (previewTimer.current) clearInterval(previewTimer.current);
    };
  }, [state]);

  const isPtt = settingsRef.current.recording_mode !== "toggle";

  return (
    <div className={`hud hud--${state}`} data-tauri-drag-region>
      <div className="hud__mic-wrap">
        {state === "recording" && <div className="hud__pulse" />}
        <div className="hud__mic-icon">
          {state === "transcribing" ? (
            <SpinnerIcon />
          ) : state === "done" ? (
            <CheckIcon />
          ) : state === "error" ? (
            <ErrorIcon />
          ) : (
            <MicIcon active={state === "recording"} />
          )}
        </div>
      </div>

      <div className="hud__center">
        {state === "recording" && (
          <>
            <div className="hud__row">
              <div className="hud__waveform">
                {levels.map((lvl, i) => (
                  <div
                    key={i}
                    className="hud__bar"
                    style={{ "--level": lvl } as React.CSSProperties}
                  />
                ))}
              </div>
              <div className="hud__timer">{formatTime(elapsedSecs)}</div>
            </div>
            {previewText && (
              <div className="hud__preview" title={previewText}>
                {previewText.length > 70 ? "…" + previewText.slice(-67) : previewText}
              </div>
            )}
          </>
        )}
        {state === "transcribing" && (
          <span className="hud__label hud__label--accent">Transcribing…</span>
        )}
        {state === "done" && (
          <span className="hud__result" title={lastText}>
            {lastText.length > 58 ? lastText.slice(0, 55) + "…" : lastText}
          </span>
        )}
        {state === "error" && (
          <span className="hud__result hud__result--error" title={lastText}>
            {lastText.length > 52 ? lastText.slice(0, 49) + "…" : lastText}
          </span>
        )}
        {state === "idle" && (
          <span className="hud__hint">
            {isPtt ? (
              <>
                <kbd>Hold</kbd> shortcut to talk
              </>
            ) : (
              <>
                <kbd>Press</kbd> shortcut to start
              </>
            )}
          </span>
        )}
      </div>

      <div className="hud__right">
        {state === "recording" && <div className="hud__rec-dot" />}
        {state === "done" && <div className="hud__done-dot" />}
      </div>
    </div>
  );
}

function formatTime(secs: number) {
  const m = Math.floor(secs / 60).toString().padStart(2, "0");
  const s = (secs % 60).toString().padStart(2, "0");
  return `${m}:${s}`;
}

function MicIcon({ active }: { active: boolean }) {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" fill={active ? "currentColor" : "none"} />
      <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
      <line x1="12" y1="19" x2="12" y2="23" />
      <line x1="8" y1="23" x2="16" y2="23" />
    </svg>
  );
}

function SpinnerIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="spin">
      <path d="M21 12a9 9 0 1 1-6.219-8.56" />
    </svg>
  );
}

function CheckIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
      <polyline points="20 6 9 17 4 12" />
    </svg>
  );
}

function ErrorIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="12" cy="12" r="10" />
      <line x1="12" y1="8" x2="12" y2="12" />
      <line x1="12" y1="16" x2="12.01" y2="16" />
    </svg>
  );
}
