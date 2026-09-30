import brandIcon from "../assets/brand.svg";

// Реальный список результатов (шаг 5 Фазы 2): ввод с debounce 50 мс, пустой
// запрос — recents, клавиатура ↑↓ Enter Esc (Esc → команда hide_window),
// иконки через convertFileSrc (D7), merge search://updated по queryId —
// устаревшие отбрасываются (D8), статусбар с прогрессом индекса.
// Риск 5: memo-строки + батчинг применений выдачи по rAF; список ограничен
// 50 элементами на стороне core — виртуализация не нужна.
import { memo, useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import {
  getIndexStatus,
  hideWindow,
  onIconsUpdated,
  onIndexProgress,
  onSearchUpdated,
  runItem,
  search,
} from "../ipc/client";
import type { IndexStatus, SearchItem } from "../ipc/types";

interface Props {
  onOpenSettings: () => void;
}

const DEBOUNCE_MS = 50;

/** Бейдж провайдера (null — без бейджа: apps/calc говорят сами за себя). */
function providerBadge(provider: SearchItem["provider"]): string | null {
  switch (provider) {
    case "files":
      return "файл";
    case "settings":
      return "параметр";
    case "system":
      return "система";
    case "web":
      return "интернет";
    default:
      return null;
  }
}

/** Иконка 20×20: PNG из кэша (D7) или буквенная заглушка. */
const ResultIcon = memo(function ResultIcon({ item }: { item: SearchItem }) {
  if (item.iconPath) {
    return (
      <img
        className="result-icon"
        src={convertFileSrc(item.iconPath)}
        alt=""
        width={20}
        height={20}
      />
    );
  }
  const letter = [...item.title][0] ?? "·";
  return (
    <span className="result-icon result-icon-fallback" aria-hidden="true">
      {letter}
    </span>
  );
});

interface RowProps {
  item: SearchItem;
  selected: boolean;
  onSelect: () => void;
  onRun: () => void;
}

/** Строка результата (memo — ререндер только при смене выделения, риск 5). */
const ResultRow = memo(function ResultRow({ item, selected, onSelect, onRun }: RowProps) {
  const badge = providerBadge(item.provider);
  return (
    // eslint-disable-next-line jsx-a11y/click-events-have-key-events -- клавиатура обрабатывается на уровне окна (↑↓ Enter)
    <li
      className={selected ? "result selected" : "result"}
      onMouseEnter={onSelect}
      onClick={onRun}
    >
      <ResultIcon item={item} />
      <span className="result-main">
        <span className="result-title">{item.title}</span>
        {item.subtitle && <span className="result-subtitle">{item.subtitle}</span>}
      </span>
      {badge && <span className="result-badge">{badge}</span>}
    </li>
  );
});

export default function ResultList({ onOpenSettings }: Props) {
  const [query, setQuery] = useState("");
  const [items, setItems] = useState<SearchItem[]>([]);
  const [selected, setSelected] = useState(0);
  const [status, setStatus] = useState<IndexStatus | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const lastQueryId = useRef(0);

  // --- Применение выдачи с батчингом по rAF (риск 5) ---
  const pendingItems = useRef<SearchItem[] | null>(null);
  const rafHandle = useRef(0);
  const applyItems = useCallback((next: SearchItem[]) => {
    pendingItems.current = next;
    if (rafHandle.current) return;
    rafHandle.current = requestAnimationFrame(() => {
      rafHandle.current = 0;
      const applied = pendingItems.current;
      pendingItems.current = null;
      if (!applied) return;
      setItems(applied);
      setSelected((sel) => (applied.length ? Math.min(sel, applied.length - 1) : 0));
    });
  }, []);
  useEffect(
    () => () => {
      if (rafHandle.current) cancelAnimationFrame(rafHandle.current);
    },
    [],
  );

  // --- События core: search://updated (merge по queryId) + index://progress ---
  useEffect(() => {
    let disposed = false;
    const unsubs: Array<() => void> = [];
    const track = (p: Promise<() => void>) =>
      p.then((un) => {
        if (disposed) un();
        else unsubs.push(un);
      });

    track(
      onSearchUpdated((resp) => {
        if (resp.queryId < lastQueryId.current) return; // устаревший ответ (D8)
        lastQueryId.current = resp.queryId;
        applyItems(resp.items);
      }),
    );
    track(onIndexProgress(setStatus));

    return () => {
      disposed = true;
      unsubs.forEach((un) => un());
    };
  }, [applyItems]);

  // Начальный статус индекса (до первого события).
  useEffect(() => {
    getIndexStatus()
      .then(setStatus)
      .catch(() => {});
  }, []);

  // --- Запросы: debounce 50 мс; пустой запрос → recents (search("")) ---
  const queryRef = useRef(query);
  queryRef.current = query;

  // Один путь выполнения запроса: и debounce ввода, и icons://updated идут
  // через обычную команду search() (валидный qid, обычное отсечение устаревших).
  const runSearch = useCallback(() => {
    const q = queryRef.current;
    search(q)
      .then((resp) => {
        if (resp.queryId < lastQueryId.current) return; // уже есть свежее (событие)
        lastQueryId.current = resp.queryId;
        applyItems(resp.items);
      })
      .catch(() => {});
  }, [applyItems]);

  useEffect(() => {
    const t = setTimeout(runSearch, DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [query, runSearch]);

  // --- icons://updated: в кэш досыпались иконки (без списка) ---
  // Тихо перезапрашиваем ТЕКУЩИЙ запрос, чтобы иконки подставились без ввода;
// всплески сливаем (извлечение идёт пачками — не чаще раза в 150 мс).
  const iconsTimer = useRef(0);
  useEffect(() => {
    let disposed = false;
    const unsubs: Array<() => void> = [];
    const track = (p: Promise<() => void>) =>
      p.then((un) => {
        if (disposed) un();
        else unsubs.push(un);
      });
    track(
      onIconsUpdated(() => {
        if (iconsTimer.current) return;
        iconsTimer.current = window.setTimeout(() => {
          iconsTimer.current = 0;
          runSearch();
        }, 150);
      }),
    );
    return () => {
      disposed = true;
      unsubs.forEach((un) => un());
      if (iconsTimer.current) {
        clearTimeout(iconsTimer.current);
        iconsTimer.current = 0;
      }
    };
  }, [runSearch]);

  // --- Действия ---
  const runSelected = useCallback((item: SearchItem | undefined) => {
    if (!item) return;
    runItem(item.id)
      .then(() => {
        // Запуск успешен: скрываем окно и сбрасываем запрос (следующий показ —
        // с чистым вводом; пустой запрос покажет обновлённые recents).
        hideWindow().catch(() => {});
        setQuery("");
      })
      .catch(() => {
        // Ошибка действия (файл исчез и т.п.) — окно остаётся открытым.
      });
  }, []);

  // --- Клавиатура: ↑↓ Enter Esc ---
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelected((s) => (items.length ? (s + 1) % items.length : 0));
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelected((s) => (items.length ? (s - 1 + items.length) % items.length : 0));
      } else if (e.key === "Enter") {
        e.preventDefault();
        runSelected(items[selected]);
      } else if (e.key === "Escape") {
        e.preventDefault();
        hideWindow().catch(() => {});
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [items, selected, runSelected]);

  // Выбранная строка всегда видна (скролл списка > 50 не нужен, но прокрутка
  // внутри фиксированной высоты окна — да).
  useEffect(() => {
    document.querySelector(".result.selected")?.scrollIntoView({ block: "nearest" });
  }, [selected, items]);

  // После скрытия (blur) и повторного показа хоткеем — фокус и выделение ввода.
  useEffect(() => {
    const onFocus = () => {
      inputRef.current?.focus();
      inputRef.current?.select();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, []);

  const indexHint = !status || status.phase === "done"
    ? "Alt+Space — показать/скрыть · Esc — скрыть"
    : status.phase === "scanning"
      ? "Индексация: сканирование папок…"
      : status.phase === "cleaning"
        ? "Индексация: проверка удалённых…"
        : `Индексация: ${status.done}/${status.total}`;

  return (
    <main className="launcher">
      <header className="searchbar" data-tauri-drag-region>
        <img className="brand-icon" src={brandIcon} alt="Iskra" draggable={false} />
        <input
          ref={inputRef}
          className="search-input"
          type="text"
          placeholder="Поиск приложений, файлов, настроек…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          autoFocus
          spellCheck={false}
          aria-label="Поиск"
        />
        <button className="icon-btn" title="Настройки" aria-label="Настройки" onClick={onOpenSettings}>
          ⚙
        </button>
      </header>
      <ul className="results" aria-label="results">
        {items.length === 0 ? (
          <li className="result-empty">
            {query.trim() ? "Ничего не найдено" : "Недавних запусков пока нет"}
          </li>
        ) : (
          items.map((item, i) => (
            <ResultRow
              key={item.id}
              item={item}
              selected={i === selected}
              onSelect={() => setSelected(i)}
              onRun={() => runSelected(item)}
            />
          ))
        )}
      </ul>
      <footer className="statusbar">{indexHint}</footer>
    </main>
  );
}
