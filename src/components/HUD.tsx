import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./HUD.css";

// The recording lifecycle now lives entirely in Rust (a single serialized
// coordinator driven directly by the shortcut press/release). This component
// is a pure display of the `hud-state` events it emits — it never decides when
// to start/stop, and never shows or hides its own window. That removes the
// whole class of frontend races that used to leave the overlay stuck open.
type Phase = "idle" | "recording" | "transcribing" | "result" | "error";

interface HudState {
  phase: Phase;
  text: string;
  show_text: boolean;
}

const WAVE_BARS = 5;

export default function HUD() {
  const [phase, setPhase] = useState<Phase>("idle");
  const [text, setText] = useState("");
  const [showText, setShowText] = useState(false);
  const [levels, setLevels] = useState<number[]>(Array(WAVE_BARS).fill(0));
  const [elapsed, setElapsed] = useState(0);
  const [preview, setPreview] = useState("");
  const livePreviewRef = useRef(false);

  useEffect(() => {
    invoke<{ live_preview?: boolean }>("get_settings")
      .then((s) => {
        livePreviewRef.current = !!s.live_preview;
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    const un = listen<HudState>("hud-state", ({ payload }) => {
      setPhase(payload.phase);
      setText(payload.text);
      setShowText(payload.show_text);
      if (payload.phase === "recording") {
        setElapsed(0);
        setPreview("");
        setLevels(Array(WAVE_BARS).fill(0));
      }
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  // Poll audio level (waveform) + elapsed + optional live preview, only while
  // actively recording.
  useEffect(() => {
    if (phase !== "recording") return;
    const levelTimer = setInterval(async () => {
      try {
        const lvl = await invoke<number>("get_audio_level");
        setLevels((prev) => [...prev.slice(1), Math.min(lvl * 12, 1)]);
      } catch {
        /* ignore */
      }
    }, 80);
    const clockTimer = setInterval(() => setElapsed((e) => e + 1), 1000);
    let previewTimer: ReturnType<typeof setInterval> | undefined;
    if (livePreviewRef.current) {
      previewTimer = setInterval(async () => {
        try {
          const t = await invoke<string>("get_partial_transcript");
          if (t) setPreview(t);
        } catch {
          /* ignore */
        }
      }, 1500);
    }
    return () => {
      clearInterval(levelTimer);
      clearInterval(clockTimer);
      if (previewTimer) clearInterval(previewTimer);
    };
  }, [phase]);

  const showResultText = phase === "result" && showText && text && text !== "No speech detected";

  return (
    <div className={`hud hud--${phase}`} data-tauri-drag-region>
      <div className="hud__pill">
        <div className="hud__lead">
          {phase === "transcribing" ? (
            <span className="hud__dots">
              <i />
              <i />
              <i />
            </span>
          ) : phase === "result" ? (
            <span className="hud__glyph hud__glyph--ok">
              <CheckIcon />
            </span>
          ) : phase === "error" ? (
            <span className="hud__glyph hud__glyph--err">
              <ErrorIcon />
            </span>
          ) : (
            <span className={`hud__orb ${phase === "recording" ? "hud__orb--live" : ""}`} />
          )}
        </div>

        <div className="hud__body">
          {phase === "recording" && (
            <div className="hud__wave">
              {levels.map((lvl, i) => (
                <span
                  key={i}
                  className="hud__wbar"
                  style={{ "--l": lvl } as React.CSSProperties}
                />
              ))}
            </div>
          )}
          {phase === "transcribing" && <span className="hud__text">Transcribing</span>}
          {phase === "result" &&
            (showResultText ? (
              <span className="hud__text hud__text--result" title={text}>
                {text.trim()}
              </span>
            ) : (
              <span className="hud__text hud__text--dim">
                {text === "No speech detected" ? "No speech" : "Done"}
              </span>
            ))}
          {phase === "error" && (
            <span className="hud__text hud__text--err" title={text}>
              {text}
            </span>
          )}
        </div>

        {phase === "recording" && <span className="hud__time">{fmt(elapsed)}</span>}
      </div>

      {phase === "recording" && preview && (
        <div className="hud__preview" title={preview}>
          {preview.length > 90 ? "…" + preview.slice(-87) : preview}
        </div>
      )}
    </div>
  );
}

function fmt(secs: number) {
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}

function CheckIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round">
      <polyline points="20 6 9 17 4 12" />
    </svg>
  );
}

function ErrorIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
      <line x1="12" y1="7" x2="12" y2="13" />
      <line x1="12" y1="17" x2="12.01" y2="17" />
    </svg>
  );
}
