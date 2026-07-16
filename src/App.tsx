import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import HUD from "./components/HUD";
import Settings from "./components/Settings";

export default function App() {
  const [view, setView] = useState<"hud" | "settings" | null>(null);

  useEffect(() => {
    const win = getCurrentWindow();
    setView(win.label === "settings" ? "settings" : "hud");
  }, []);

  if (view === null) return null;
  return view === "settings" ? <Settings /> : <HUD />;
}
