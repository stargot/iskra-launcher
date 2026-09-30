// Экран настроек (шаг 5): тема (мгновенно), хоткей (input + Сохранить,
// HotkeyBusy → инлайн-ошибка + фактический хоткей из get_runtime_info),
// автозапуск (checkbox, состояние из Settings).
import { useEffect, useState } from "react";
import { getRuntimeInfo, onHotkeyChanged, updateSettings } from "../ipc/client";
import type { RuntimeInfo, Settings, SettingsError, Theme } from "../ipc/types";

interface Props {
  settings: Settings;
  onBack: () => void;
}

function describeError(err: SettingsError): string {
  if (typeof err === "string") {
    return err === "hotkeyBusy"
      ? "Хоткей занят другим приложением — настройки не изменились."
      : "Некорректная комбинация хоткея (пример: Ctrl+Alt+K).";
  }
  return `Ошибка сохранения: ${err.io.message}`;
}

function isSettingsError(err: unknown): err is SettingsError {
  return (
    typeof err === "string" ||
    (typeof err === "object" && err !== null && "io" in (err as Record<string, unknown>))
  );
}

export default function SettingsView({ settings, onBack }: Props) {
  const [hotkeyDraft, setHotkeyDraft] = useState(settings.hotkey);
  const [runtime, setRuntime] = useState<RuntimeInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  // Фактический хоткей + отслеживание фолбэка.
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    getRuntimeInfo()
      .then((info) => {
        if (!disposed) setRuntime(info);
      })
      .catch(() => {});
    onHotkeyChanged((changed) => {
      getRuntimeInfo().then(setRuntime).catch(() => {});
      if (changed.fromFallback) {
        setError(
          `«${changed.hotkey}» зарегистрирован как фолбэк — основной хоткей занят.`,
        );
      }
    }).then((un) => {
      if (disposed) un();
      else unlisten = un;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  // Внешние изменения настроек (трей/другой источник) обновляют поле ввода.
  useEffect(() => {
    setHotkeyDraft(settings.hotkey);
  }, [settings.hotkey]);

  const changeTheme = (theme: Theme) => {
    if (theme === settings.theme) return;
    updateSettings({ theme }).catch((err) => setError(describeError(err as SettingsError)));
  };

  const saveHotkey = async () => {
    setError(null);
    setNotice(null);
    if (hotkeyDraft === settings.hotkey) return;
    try {
      const next = await updateSettings({ hotkey: hotkeyDraft });
      setNotice(`Хоткей сохранён: ${next.hotkey}.`);
    } catch (err) {
      if (isSettingsError(err)) setError(describeError(err));
      else setError(String(err));
      // Откат поля + показать фактический (план: HotkeyBusy → инлайн + фактический).
      setHotkeyDraft(settings.hotkey);
    }
    try {
      setRuntime(await getRuntimeInfo());
    } catch {
      /* некритично */
    }
  };

  const toggleAutostart = (enabled: boolean) => {
    setError(null);
    updateSettings({ autostart: enabled }).catch((err) =>
      setError(describeError(err as SettingsError)),
    );
  };

  return (
    <main className="settings">
      <header className="topbar" data-tauri-drag-region>
        <button className="icon-btn" title="Назад" aria-label="Назад" onClick={onBack}>
          ←
        </button>
        <span className="brand">Настройки</span>
        <span className="spacer" />
      </header>

      <div className="settings-scroll">
        <section className="card">
          <span className="card-title">Тема</span>
          <div className="segmented" role="group" aria-label="Тема">
            <button
              className={settings.theme === "dark" ? "active" : ""}
              onClick={() => changeTheme("dark")}
            >
              Тёмная
            </button>
            <button
              className={settings.theme === "light" ? "active" : ""}
              onClick={() => changeTheme("light")}
            >
              Светлая
            </button>
          </div>
        </section>

        <section className="card">
          <span className="card-title">Глобальный хоткей показа</span>
          <div className="row">
            <input
              type="text"
              value={hotkeyDraft}
              spellCheck={false}
              placeholder="Ctrl+Alt+K"
              aria-label="Хоткей"
              onChange={(e) => setHotkeyDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void saveHotkey();
              }}
            />
            <button className="primary" onClick={() => void saveHotkey()}>
              Сохранить
            </button>
          </div>
          {runtime && (
            <span className="inline-note">
              Сейчас зарегистрирован:{" "}
              <span className="kbd">{runtime.activeHotkey ?? "— нет —"}</span>
              {runtime.fallbackActive && " (фолбэк: основной занят)"}
            </span>
          )}
          {error && <span className="inline-error">{error}</span>}
          {!error && notice && <span className="inline-ok">{notice}</span>}
        </section>

        <section className="card">
          <span className="card-title">Система</span>
          <label className="check-row">
            <input
              type="checkbox"
              checked={settings.autostart}
              onChange={(e) => toggleAutostart(e.target.checked)}
            />
            <span>Запускать Iskra при входе в Windows</span>
          </label>
          <span className="inline-note">
            Управляется через HKCU\…\CurrentVersion\Run (значение «Iskra»).
          </span>
        </section>
      </div>
    </main>
  );
}
