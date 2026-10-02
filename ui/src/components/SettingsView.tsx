// Экран настроек (шаг 5): тема (мгновенно), хоткей (input + Сохранить,
// HotkeyBusy → инлайн-ошибка + фактический хоткей из get_runtime_info),
// автозапуск (checkbox, состояние из Settings).
// Фаза 3, шаг 5: секция «Клипборд» — toggle мониторинга (сразу через
// update_settings, D7) и исключения (textarea, по process name на строку);
// секция «Сниппеты» — CRUD (имя/тело/keywords, D9; вставка из поиска — run_item).
import { useCallback, useEffect, useState } from "react";
import {
  getRuntimeInfo,
  onHotkeyChanged,
  snippetCreate,
  snippetDelete,
  snippetsList,
  snippetUpdate,
  updateSettings,
} from "../ipc/client";
import type { RuntimeInfo, Settings, SettingsError, Snippet, Theme, WindowMode } from "../ipc/types";

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

/** Строка сниппета: локальный черновик, Сохранить активна только при изменениях. */
function SnippetRow({
  snippet,
  onSaved,
  onError,
  onNotice,
}: {
  snippet: Snippet;
  onSaved: () => void;
  onError: (message: string) => void;
  onNotice: (message: string) => void;
}) {
  const [name, setName] = useState(snippet.name);
  const [body, setBody] = useState(snippet.body);
  const [keywords, setKeywords] = useState(snippet.keywords);
  const dirty = name !== snippet.name || body !== snippet.body || keywords !== snippet.keywords;

  const save = async () => {
    try {
      await snippetUpdate(snippet.id, name, body, keywords);
      onNotice(`Сниппет «${name.trim()}» сохранён.`);
      onSaved();
    } catch (err) {
      onError(err instanceof Error ? err.message : String(err));
    }
  };

  const remove = async () => {
    try {
      await snippetDelete(snippet.id);
      onNotice(`Сниппет «${snippet.name}» удалён.`);
      onSaved();
    } catch (err) {
      onError(err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <div className="snippet-row">
      <div className="row">
        <input
          className="grow"
          type="text"
          value={name}
          placeholder="Имя (например, Адрес)"
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && dirty) void save();
          }}
          spellCheck={false}
          aria-label="Имя сниппета"
        />
        <input
          type="text"
          value={keywords}
          placeholder="Ключевые слова"
          onChange={(e) => setKeywords(e.target.value)}
          spellCheck={false}
          aria-label="Ключевые слова сниппета"
        />
      </div>
      <textarea
        rows={2}
        value={body}
        placeholder="Тело сниппета — вставится как есть"
        onChange={(e) => setBody(e.target.value)}
        spellCheck={false}
        aria-label="Тело сниппета"
      />
      <div className="row">
        <button className="primary" disabled={!dirty} onClick={() => void save()}>
          Сохранить
        </button>
        <button title="Удалить сниппет" onClick={() => void remove()}>
          Удалить
        </button>
      </div>
    </div>
  );
}

export default function SettingsView({ settings, onBack }: Props) {
  const [hotkeyDraft, setHotkeyDraft] = useState(settings.hotkey);
  const [runtime, setRuntime] = useState<RuntimeInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  // Клипборд: черновик исключений (по process name на строку).
  const [exclusionsDraft, setExclusionsDraft] = useState(
    settings.clipboardExcludedApps.join("\n"),
  );
  // Сниппеты: список + форма добавления.
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [newName, setNewName] = useState("");
  const [newBody, setNewBody] = useState("");
  const [newKeywords, setNewKeywords] = useState("");

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

  // Исключения клипборда: синхронизация с внешними изменениями настроек.
  useEffect(() => {
    setExclusionsDraft(settings.clipboardExcludedApps.join("\n"));
  }, [settings.clipboardExcludedApps]);

  const reloadSnippets = useCallback(() => {
    snippetsList()
      .then(setSnippets)
      .catch(() => {});
  }, []);

  useEffect(() => {
    reloadSnippets();
  }, [reloadSnippets]);

  const changeTheme = (theme: Theme) => {
    if (theme === settings.theme) return;
    updateSettings({ theme }).catch((err) => setError(describeError(err as SettingsError)));
  };

  // Размер окна: применяется сразу (update_settings → apply_mode на лету, D2).
  const changeWindowMode = (mode: WindowMode) => {
    if (mode === settings.windowMode) return;
    setError(null);
    updateSettings({ windowMode: mode }).catch((err) =>
      setError(describeError(err as SettingsError)),
    );
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

  // Клипборд: тумблер мониторинга — применяется сразу (D7, на лету).
  const toggleClipboard = (enabled: boolean) => {
    setError(null);
    updateSettings({ clipboardEnabled: enabled }).catch((err) =>
      setError(describeError(err as SettingsError)),
    );
  };

  // Клипборд: исключения — по одному process name на строку (D7).
  const saveExclusions = async () => {
    setError(null);
    setNotice(null);
    const apps = exclusionsDraft
      .split("\n")
      .map((s) => s.trim().toLowerCase())
      .filter(Boolean);
    try {
      const next = await updateSettings({ clipboardExcludedApps: apps });
      setExclusionsDraft(next.clipboardExcludedApps.join("\n"));
      setNotice(
        next.clipboardExcludedApps.length
          ? `Исключения сохранены: ${next.clipboardExcludedApps.join(", ")}.`
          : "Исключения очищены — история пишется от всех приложений.",
      );
    } catch (err) {
      if (isSettingsError(err)) setError(describeError(err));
      else setError(String(err));
    }
  };

  const addSnippet = async () => {
    setError(null);
    setNotice(null);
    try {
      const created = await snippetCreate(newName, newBody, newKeywords);
      setNewName("");
      setNewBody("");
      setNewKeywords("");
      setNotice(`Сниппет «${created.name}» добавлен — найдите его в поиске по имени.`);
      reloadSnippets();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
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
          <span className="card-title">Размер окна</span>
          <div className="segmented" role="group" aria-label="Размер окна">
            <button
              className={settings.windowMode === "normal" ? "active" : ""}
              onClick={() => changeWindowMode("normal")}
            >
              Обычный
            </button>
            <button
              className={settings.windowMode === "double" ? "active" : ""}
              onClick={() => changeWindowMode("double")}
            >
              2×
            </button>
            <button
              className={settings.windowMode === "fullscreen" ? "active" : ""}
              onClick={() => changeWindowMode("fullscreen")}
            >
              На весь экран
            </button>
          </div>
          <span className="inline-note">
            Применяется сразу и переживает перезапуск. 2× сжимается, если не
            влезает в экран. В fullscreen лончер по-прежнему скрывается при
            клике мимо (повторный хоткей возвращает).
          </span>
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

        <section className="card">
          <span className="card-title">Клипборд</span>
          <label className="check-row">
            <input
              type="checkbox"
              checked={settings.clipboardEnabled}
              onChange={(e) => toggleClipboard(e.target.checked)}
            />
            <span>Вести историю буфера обмена</span>
          </label>
          <span className="inline-note">
            Применяется сразу, переживает перезапуск. Лимиты: 1000 записей / 200 МБ
            (изображения ≤ 20 МБ).
          </span>
          <textarea
            rows={3}
            value={exclusionsDraft}
            placeholder={"Исключения — по одному процессу на строку:\nnotepad.exe\nexcel"}
            onChange={(e) => setExclusionsDraft(e.target.value)}
            spellCheck={false}
            aria-label="Исключённые приложения"
          />
          <div className="row">
            <button className="primary" onClick={() => void saveExclusions()}>
              Сохранить исключения
            </button>
          </div>
          <span className="inline-note">
            Копирование из исключённых приложений в историю не попадает (регистр
            и «.exe» не важны).
          </span>
        </section>

        <section className="card">
          <span className="card-title">Сниппеты</span>
          <span className="inline-note">
            Находятся в поиске по имени/ключевым словам; Enter — копирует тело и
            сразу вставляет в активное окно.
          </span>
          {snippets.length === 0 && (
            <span className="inline-note">Сниппетов пока нет — добавьте первый ниже.</span>
          )}
          {snippets.map((s) => (
            <SnippetRow
              key={s.id}
              snippet={s}
              onSaved={reloadSnippets}
              onError={setError}
              onNotice={setNotice}
            />
          ))}
          <div className="snippet-row">
            <div className="row">
              <input
                className="grow"
                type="text"
                value={newName}
                placeholder="Имя нового сниппета"
                onChange={(e) => setNewName(e.target.value)}
                spellCheck={false}
                aria-label="Имя нового сниппета"
              />
              <input
                type="text"
                value={newKeywords}
                placeholder="Ключевые слова"
                onChange={(e) => setNewKeywords(e.target.value)}
                spellCheck={false}
                aria-label="Ключевые слова нового сниппета"
              />
            </div>
            <textarea
              rows={2}
              value={newBody}
              placeholder="Тело нового сниппета"
              onChange={(e) => setNewBody(e.target.value)}
              spellCheck={false}
              aria-label="Тело нового сниппета"
            />
            <div className="row">
              <button
                className="primary"
                disabled={!newName.trim() || !newBody.trim()}
                onClick={() => void addSnippet()}
              >
                Добавить
              </button>
            </div>
          </div>
        </section>
      </div>
    </main>
  );
}
