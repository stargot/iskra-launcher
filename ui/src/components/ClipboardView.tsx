// Экран клипборда (Фаза 3, шаг 4; план §1 D8): список top-100 (pinned сверху),
// поиск по превью (debounce 50 мс), pin/удаление, Enter — вставить (цепочку D6
// — скрытие лончера, restore, Ctrl+V — делает сервис), Esc — назад в лончер
// (обрабатывает App; НЕ hide_window). Подписка clipboard://updated → перезапрос
// списка (троттлинг 100 мс — Office шлёт события пачками).
import { memo, useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import {
  clipboardDelete,
  clipboardList,
  clipboardPaste,
  clipboardPinned,
  onClipboardUpdated,
} from "../ipc/client";
import type { ClipboardEntry } from "../ipc/types";

interface Props {
  onBack: () => void;
}

const DEBOUNCE_MS = 50;
const REFRESH_THROTTLE_MS = 100;

/** Пути из files-записи (content — JSON-массив; зеркало ClipboardEntry::files). */
function entryFiles(entry: ClipboardEntry): string[] {
  if (entry.kind !== "files" || !entry.content) return [];
  try {
    const parsed: unknown = JSON.parse(entry.content);
    return Array.isArray(parsed) ? (parsed as string[]) : [];
  } catch {
    return [];
  }
}

/** Относительное время для подписи (used_at — unix-мс). */
function relTime(ts: number): string {
  const d = Date.now() - ts;
  const minute = 60_000;
  const hour = 3_600_000;
  const day = 86_400_000;
  if (d < minute) return "только что";
  if (d < hour) return `${Math.floor(d / minute)} мин назад`;
  if (d < day) return `${Math.floor(d / hour)} ч назад`;
  return `${Math.floor(d / day)} дн назад`;
}

/** Иконка записи: thumbnail 128px для изображений (D4), глиф для остальных. */
const ClipIcon = memo(function ClipIcon({ entry }: { entry: ClipboardEntry }) {
  const [broken, setBroken] = useState(false);
  // Thumbnail лежит рядом с PNG: {stem}_thumb.png (раскладка сервиса, D4).
  const thumb = entry.imagePath
    ? entry.imagePath.replace(/\.png$/i, "_thumb.png")
    : null;
  if (entry.kind === "image" && thumb && !broken) {
    return (
      <img
        className="clip-thumb"
        src={convertFileSrc(thumb)}
        alt=""
        width={36}
        height={36}
        loading="lazy"
        onError={() => setBroken(true)}
      />
    );
  }
  const glyph = entry.kind === "files" ? "📁" : entry.kind === "image" ? "🖼" : "📝";
  return (
    <span className="clip-icon" aria-hidden="true">
      {glyph}
    </span>
  );
});

/** Имя файла из пути (для чипов files-записей). */
function baseName(path: string): string {
  const i = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  return i >= 0 ? path.slice(i + 1) : path;
}

interface RowProps {
  entry: ClipboardEntry;
  selected: boolean;
  onSelect: () => void;
  onRun: () => void;
  onPin: () => void;
  onDelete: () => void;
}

/** Строка истории (memo — ререндер при смене выделения). */
const ClipRow = memo(function ClipRow({ entry, selected, onSelect, onRun, onPin, onDelete }: RowProps) {
  const files = entry.kind === "files" ? entryFiles(entry) : [];
  return (
    // eslint-disable-next-line jsx-a11y/click-events-have-key-events -- клавиатура на уровне окна (↑↓ Enter Del)
    <li
      className={selected ? "clip-row selected" : "clip-row"}
      onMouseEnter={onSelect}
      onClick={onRun}
    >
      <ClipIcon entry={entry} />
      <span className="clip-main">
        {entry.kind === "files" ? (
          <span className="clip-chips">
            {files.map((f) => (
              <span key={f} className="clip-chip">
                {baseName(f)}
              </span>
            ))}
          </span>
        ) : (
          <span className="clip-text">{entry.preview}</span>
        )}
        <span className="clip-meta">
          {entry.sourceApp ?? "источник неизвестен"} · {relTime(entry.usedAt)}
          {entry.usedCount > 1 && ` · ×${entry.usedCount}`}
        </span>
      </span>
      <span className="clip-actions">
        <button
          className={entry.pinned ? "active" : ""}
          title={entry.pinned ? "Открепить" : "Закрепить"}
          aria-label={entry.pinned ? "Открепить" : "Закрепить"}
          onClick={(e) => {
            e.stopPropagation();
            onPin();
          }}
        >
          📌
        </button>
        <button
          title="Удалить"
          aria-label="Удалить"
          onClick={(e) => {
            e.stopPropagation();
            onDelete();
          }}
        >
          ✕
        </button>
      </span>
    </li>
  );
});

export default function ClipboardView({ onBack }: Props) {
  const [entries, setEntries] = useState<ClipboardEntry[]>([]);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const queryRef = useRef(query);
  queryRef.current = query;

  // Один путь запроса: debounce ввода, события и действия идут через refresh.
  const refresh = useCallback(() => {
    const q = queryRef.current.trim();
    clipboardList(q || null)
      .then((list) => {
        setEntries(list);
        setSelected((sel) => (list.length ? Math.min(sel, list.length - 1) : 0));
      })
      .catch(() => {});
  }, []);

  // Начальная загрузка + debounce ввода (50 мс, как в ResultList).
  useEffect(() => {
    const t = setTimeout(refresh, DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [query, refresh]);

  // clipboard://updated (запись/дедуп/удаление/pin) → тихий перезапрос списка.
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    let timer = 0;
    onClipboardUpdated(() => {
      if (timer) return;
      timer = window.setTimeout(() => {
        timer = 0;
        refresh();
      }, REFRESH_THROTTLE_MS);
    }).then((un) => {
      if (disposed) un();
      else unlisten = un;
    });
    return () => {
      disposed = true;
      unlisten?.();
      if (timer) {
        clearTimeout(timer);
        timer = 0;
      }
    };
  }, [refresh]);

  const pasteEntry = useCallback(
    (entry: ClipboardEntry | undefined) => {
      if (!entry) return;
      // Лончер скрывает сам сервис (цепочка D6) — hide_window не зовём.
      clipboardPaste(entry.id).then(refresh).catch(() => {});
    },
    [refresh],
  );

  const pinEntry = useCallback(
    (entry: ClipboardEntry | undefined) => {
      if (!entry) return;
      clipboardPinned(entry.id, !entry.pinned).then(refresh).catch(() => {});
    },
    [refresh],
  );

  const deleteEntry = useCallback(
    (entry: ClipboardEntry | undefined) => {
      if (!entry) return;
      clipboardDelete(entry.id).then(refresh).catch(() => {});
    },
    [refresh],
  );

  // Клавиатура: ↑↓ Enter Del (Esc — на уровне App: назад в лончер).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelected((s) => (entries.length ? (s + 1) % entries.length : 0));
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelected((s) => (entries.length ? (s - 1 + entries.length) % entries.length : 0));
      } else if (e.key === "Enter") {
        e.preventDefault();
        pasteEntry(entries[selected]);
      } else if (e.key === "Delete") {
        e.preventDefault();
        deleteEntry(entries[selected]);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [entries, selected, pasteEntry, deleteEntry]);

  // Выбранная строка всегда видна.
  useEffect(() => {
    document.querySelector(".clip-row.selected")?.scrollIntoView({ block: "nearest" });
  }, [selected, entries]);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  return (
    <main className="clipboard">
      <header className="topbar" data-tauri-drag-region>
        <button className="icon-btn" title="Назад" aria-label="Назад" onClick={onBack}>
          ←
        </button>
        <input
          ref={inputRef}
          className="search-input"
          type="text"
          placeholder="Поиск по клипборду…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          spellCheck={false}
          aria-label="Поиск по клипборду"
        />
        <span className="hint">Enter — вставить · Del — удалить</span>
      </header>
      <ul className="clip-list" aria-label="clipboard history">
        {entries.length === 0 ? (
          <li className="result-empty">
            {query.trim() ? "Ничего не найдено" : "История пуста — скопируйте что-нибудь"}
          </li>
        ) : (
          entries.map((entry, i) => (
            <ClipRow
              key={entry.id}
              entry={entry}
              selected={i === selected}
              onSelect={() => setSelected(i)}
              onRun={() => pasteEntry(entry)}
              onPin={() => pinEntry(entry)}
              onDelete={() => deleteEntry(entry)}
            />
          ))
        )}
      </ul>
      <footer className="statusbar">
        {entries.length} записей · история переживает перезагрузку · исключения — в настройках
      </footer>
    </main>
  );
}
