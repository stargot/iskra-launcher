// Iskra UI (шаг 5): два «экрана» — список | настройки (простой state, без роутера).
// Тема применяется к <html data-theme> мгновенно и синхронизируется событием
// settings://changed (изменения могут приходить и из трея).
import { useEffect, useState } from "react";
import ResultList from "./components/ResultList";
import SettingsView from "./components/SettingsView";
import { getSettings, onNavSettings, onSettingsChanged } from "./ipc/client";
import type { Settings } from "./ipc/types";

type Screen = "launcher" | "settings";

export default function App() {
  const [screen, setScreen] = useState<Screen>("launcher");
  const [settings, setSettings] = useState<Settings | null>(null);

  // Загрузка настроек + подписки на события core (nav из трея, settings://changed).
  useEffect(() => {
    let disposed = false;
    const unsubs: Array<() => void> = [];
    const track = (p: Promise<() => void>) =>
      p.then((un) => {
        if (disposed) un();
        else unsubs.push(un);
      });

    getSettings()
      .then((s) => {
        if (!disposed) setSettings(s);
      })
      .catch(() => {});
    track(onSettingsChanged(setSettings));
    track(onNavSettings(() => setScreen("settings")));

    return () => {
      disposed = true;
      unsubs.forEach((un) => un());
    };
  }, []);

  // Тема: мгновенное применение на <html> (data-theme читает theme.css).
  useEffect(() => {
    document.documentElement.dataset.theme = settings?.theme ?? "dark";
  }, [settings]);

  // Esc на настройках — назад к списку.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && screen === "settings") setScreen("launcher");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [screen]);

  if (screen === "settings" && settings) {
    return <SettingsView settings={settings} onBack={() => setScreen("launcher")} />;
  }
  return <ResultList onOpenSettings={() => setScreen("settings")} />;
}
