// Iskra UI (шаг 5 Фазы 1 + Фаза 3 + прогон 2): четыре «экрана» — список |
// настройки | клипборд | сниппеты (простой state, без роутера). Тема применяется к <html data-theme>
// мгновенно и синхронизируется событием settings://changed (изменения могут
// приходить и из трея). Esc на настройках/клипборде — назад к списку (НЕ
// hide_window: скрытие лончера на экране клипборда делает только paste).
import { useCallback, useEffect, useState } from "react";
import ClipboardView from "./components/ClipboardView";
import ResultList from "./components/ResultList";
import SnippetsView from "./components/SnippetsView";
import SettingsView from "./components/SettingsView";
import { getSettings, onNavSettings, onSettingsChanged } from "./ipc/client";
import type { Settings } from "./ipc/types";

type Screen = "launcher" | "settings" | "clipboard" | "snippets";

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

  // Esc на втором экране — назад к списку (скрытие окна — только hide_window
  // из ResultList; paste на клипборде скрывает окно сам — сервис).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && screen !== "launcher") setScreen("launcher");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [screen]);

  const openClipboard = useCallback(() => setScreen("clipboard"), []);
  const openSnippets = useCallback(() => setScreen("snippets"), []);

  if (screen === "settings" && settings) {
    return <SettingsView settings={settings} onBack={() => setScreen("launcher")} />;
  }
  if (screen === "clipboard") {
    return <ClipboardView onBack={() => setScreen("launcher")} />;
  }
  if (screen === "snippets") {
    return <SnippetsView onBack={() => setScreen("launcher")} />;
  }
  return (
    <ResultList
      onOpenSettings={() => setScreen("settings")}
      onOpenClipboard={openClipboard}
      onOpenSnippets={openSnippets}
    />
  );
}
