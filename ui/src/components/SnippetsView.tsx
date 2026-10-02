// Экран сниппетов (прогон 2, D7): CRUD переехал из настроек на четвёртый
// экран. Мастер-список с локальным фильтром по имени/keywords + панель
// редактора (имя, keywords, моно-textarea rows 12, resize: vertical) с
// dirty-состоянием и живым превью с подсветкой (debounce 150 мс, лимит и
// режимы — lib/highlight.ts). Ошибки команд приходят человекочитаемыми
// строками (SnippetError::to_string). Esc — назад в лончер (обрабатывает
// App, как на клипборде). Вставка из общего поиска не менялась (D10).
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  snippetCreate,
  snippetDelete,
  snippetsList,
  snippetUpdate,
} from "../ipc/client";
import type { Snippet } from "../ipc/types";
import { Highlighted } from "../lib/highlight";

interface Props {
  onBack: () => void;
}

const PREVIEW_DEBOUNCE_MS = 150;
const DELETE_CONFIRM_MS = 3000;

/** Черновик редактора: id = null — новый, ещё не сохранённый сниппет. */
interface Draft {
  id: number | null;
  name: string;
  body: string;
  keywords: string;
}

const NEW_DRAFT: Draft = { id: null, name: "", body: "", keywords: "" };

/** Черновик из сохранённого сниппета. */
function draftOf(s: Snippet): Draft {
  return { id: s.id, name: s.name, body: s.body, keywords: s.keywords };
}

export default function SnippetsView({ onBack }: Props) {
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [filter, setFilter] = useState("");
  const [draft, setDraft] = useState<Draft | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  // Тело попадает в подсвечиваемое превью с debounce 150 мс (риск 6 —
  // подсветка не должна латентить ввод в textarea).
  const [previewBody, setPreviewBody] = useState("");
  // Двухклик-гард удаления (ревью 2): первый клик — «Точно удалить?», второй
  // в течение ~3 с — удаление, иначе сброс.
  const [confirmDelete, setConfirmDelete] = useState(false);
  const deleteTimer = useRef(0);

  const reload = useCallback(() => {
    snippetsList()
      .then(setSnippets)
      .catch(() => {});
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  useEffect(() => {
    const t = setTimeout(() => setPreviewBody(draft?.body ?? ""), PREVIEW_DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [draft?.body]);

  // Локальный фильтр списка: подстрока в имени или keywords (без учета регистра).
  const visible = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return snippets;
    return snippets.filter(
      (s) => s.name.toLowerCase().includes(q) || s.keywords.toLowerCase().includes(q),
    );
  }, [snippets, filter]);

  const source = draft?.id !== null && draft ? snippets.find((s) => s.id === draft.id) : undefined;
  const dirty =
    !!draft &&
    (draft.id === null ||
      draft.name !== source?.name ||
      draft.body !== source?.body ||
      draft.keywords !== source?.keywords);
  // Валидация зеркалит core (пустые имя/тело отклоняются — SnippetError).
  const filled = !!draft && !!draft.name.trim() && !!draft.body.trim();

  const resetDeleteConfirm = useCallback(() => {
    if (deleteTimer.current) {
      clearTimeout(deleteTimer.current);
      deleteTimer.current = 0;
    }
    setConfirmDelete(false);
  }, []);

  // Анмаунт экрана — гасим таймер гарда удаления.
  useEffect(() => resetDeleteConfirm, [resetDeleteConfirm]);

  // Ревью 2: клик по другому сниппету и «Новый» не должны молча терять правки.
  const hasContent =
    !!draft && (!!draft.name.trim() || !!draft.body.trim() || !!draft.keywords.trim());

  const confirmDiscard = (): boolean => {
    if (!draft || !dirty || !hasContent) return true;
    return window.confirm("Есть несохранённые изменения. Уйти без сохранения?");
  };

  const openSnippet = (s: Snippet) => {
    if (!confirmDiscard()) return;
    resetDeleteConfirm();
    setError(null);
    setNotice(null);
    setDraft(draftOf(s));
  };

  const startNew = () => {
    if (!confirmDiscard()) return;
    resetDeleteConfirm();
    setError(null);
    setNotice(null);
    setDraft({ ...NEW_DRAFT });
  };

  const save = async () => {
    if (!draft || !filled) return;
    resetDeleteConfirm();
    setError(null);
    setNotice(null);
    try {
      if (draft.id === null) {
        const created = await snippetCreate(draft.name, draft.body, draft.keywords);
        setNotice(`Сниппет «${created.name.trim()}» добавлен — найдите его в поиске по имени.`);
        setDraft(draftOf(created));
      } else {
        await snippetUpdate(draft.id, draft.name, draft.body, draft.keywords);
        setNotice(`Сниппет «${draft.name.trim()}» сохранён.`);
      }
      reload();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const remove = async () => {
    if (!draft || draft.id === null) return;
    // Первый клик — подтверждение; второй в течение ~3 с — удаление. Явное
    // подтверждение покрывает отбрасывание dirty-черновика при удалении.
    if (!confirmDelete) {
      setConfirmDelete(true);
      if (deleteTimer.current) clearTimeout(deleteTimer.current);
      deleteTimer.current = window.setTimeout(() => {
        deleteTimer.current = 0;
        setConfirmDelete(false);
      }, DELETE_CONFIRM_MS);
      return;
    }
    resetDeleteConfirm();
    setError(null);
    setNotice(null);
    try {
      await snippetDelete(draft.id);
      setNotice(`Сниппет «${draft.name.trim() || `#${draft.id}`}» удалён.`);
      setDraft(null);
      reload();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <main className="snippets">
      <header className="topbar" data-tauri-drag-region>
        <button className="icon-btn" title="Назад" aria-label="Назад" onClick={onBack}>
          ←
        </button>
        <span className="brand">Сниппеты</span>
        <span className="spacer" />
      </header>

      <div className="snippets-body">
        <aside className="snippet-list">
          <input
            type="text"
            value={filter}
            placeholder="Фильтр по имени/ключевым словам"
            onChange={(e) => setFilter(e.target.value)}
            spellCheck={false}
            aria-label="Фильтр сниппетов"
          />
          <button className="primary" onClick={startNew}>
            Новый
          </button>
          <ul aria-label="Список сниппетов">
            {visible.length === 0 ? (
              <li className="result-empty">
                {snippets.length === 0 ? "Сниппетов пока нет — создайте первый" : "Не найдено"}
              </li>
            ) : (
              visible.map((s) => (
                <li
                  key={s.id}
                  // eslint-disable-next-line jsx-a11y/click-events-have-key-events -- выбор мышью; клавиатура — поля редактора
                  className={draft?.id === s.id ? "snippet-item selected" : "snippet-item"}
                  onClick={() => openSnippet(s)}
                >
                  <span className="snippet-item-name">{s.name}</span>
                  {s.keywords && <span className="snippet-item-kw">{s.keywords}</span>}
                </li>
              ))
            )}
          </ul>
        </aside>

        <section className="snippet-editor">
          {draft === null ? (
            <div className="snippet-placeholder">
              Выберите сниппет слева или создайте новый. Сниппеты находятся в
              общем поиске по имени и ключевым словам.
            </div>
          ) : (
            <>
              <div className="row">
                <input
                  className="grow"
                  type="text"
                  value={draft.name}
                  placeholder="Имя (например, Адрес)"
                  onChange={(e) => setDraft({ ...draft, name: e.target.value })}
                  spellCheck={false}
                  aria-label="Имя сниппета"
                />
                <input
                  type="text"
                  value={draft.keywords}
                  placeholder="Ключевые слова"
                  onChange={(e) => setDraft({ ...draft, keywords: e.target.value })}
                  spellCheck={false}
                  aria-label="Ключевые слова сниппета"
                />
              </div>
              <textarea
                className="snippet-body"
                rows={12}
                value={draft.body}
                placeholder="Тело сниппета — вставится как есть"
                onChange={(e) => setDraft({ ...draft, body: e.target.value })}
                spellCheck={false}
                aria-label="Тело сниппета"
              />
              <div className="row">
                <button
                  className="primary"
                  disabled={!dirty || !filled}
                  onClick={() => void save()}
                >
                  Сохранить
                </button>
                {draft.id !== null && (
                  <button
                    title={confirmDelete ? "Нажмите ещё раз — удаление" : "Удалить сниппет"}
                    onClick={() => void remove()}
                  >
                    {confirmDelete ? "Точно удалить?" : "Удалить"}
                  </button>
                )}
                <span className="inline-note">
                  {dirty ? "Есть несохранённые изменения" : "Изменений нет"}
                </span>
              </div>
              {error && <span className="inline-error">{error}</span>}
              {!error && notice && <span className="inline-ok">{notice}</span>}
              <span className="card-title">Превью (как вставится)</span>
              <div className="snippet-preview" aria-label="Превью с подсветкой">
                <Highlighted body={previewBody} />
              </div>
            </>
          )}
        </section>
      </div>

      <footer className="statusbar">
        {snippets.length} сниппетов · ищутся в общем поиске по имени и ключевым словам · Esc — назад
      </footer>
    </main>
  );
}
