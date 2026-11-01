import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { buttonClass, primaryClass, errorText, Results, OperationResult } from './BetaControls';
import { useAppStore } from '../store/useAppStore';

interface Entry { id: string; source: string; destination: string | null; rule: string; action: string; size: number; warning: boolean; error: string | null }
interface Preview { id: string; entries: Entry[] }
interface Verdict { path: string; verdict: string; confidence: number; probabilities: Record<string, number> }
const verdictClass: Record<string, string> = {
  keep: 'bg-green-100 text-green-800 dark:bg-green-900/40 dark:text-green-200',
  trash: 'bg-red-100 text-red-800 dark:bg-red-900/40 dark:text-red-200',
  unsure: 'bg-amber-100 text-amber-800 dark:bg-amber-900/40 dark:text-amber-200',
};
export default function ReviewQueue({ paths }: { paths: string[] | null }) {
  const { t } = useTranslation();
  const [preview, setPreview] = useState<Preview | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [confirm, setConfirm] = useState(false);
  const [results, setResults] = useState<OperationResult[]>([]);
  const [verdicts, setVerdicts] = useState<Record<string, Verdict>>({});
  const [classifying, setClassifying] = useState(false);
  const [aiError, setAiError] = useState('');
  const [trashConfirm, setTrashConfirm] = useState(false);
  const currentId = useRef<string | null>(null);
  const generation = useRef(0);
  async function refresh() {
    const request = ++generation.current;
    setBusy(true); setError(''); setConfirm(false); setResults([]);
    setVerdicts({}); setAiError(''); setTrashConfirm(false);
    if (currentId.current) await invoke('discard_preview_cmd', { id: currentId.current }).catch(() => {});
    currentId.current = null; setPreview(null);
    try {
      const plan = await invoke<Preview>('preview_cmd', { paths: paths?.length ? paths : null });
      if (request !== generation.current) { await invoke('discard_preview_cmd', { id: plan.id }); return; }
      currentId.current = plan.id; setPreview(plan);
      setSelected(new Set(plan.entries.filter(e => !e.error && !e.warning && e.action !== 'delete').map(e => e.id)));
    } catch (error) { if (request === generation.current) setError(errorText(error)); }
    finally { if (request === generation.current) setBusy(false); }
  }
  useEffect(() => {
    void refresh();
    return () => { ++generation.current; if (currentId.current) void invoke('discard_preview_cmd', { id: currentId.current }); };
  }, [paths]);
  async function apply() {
    if (!preview || !selected.size) return;
    setBusy(true); setError('');
    try {
      setResults(await invoke<OperationResult[]>('apply_preview_cmd', { id: preview.id, selected: [...selected] }));
      currentId.current = null; setPreview(null); setSelected(new Set()); setConfirm(false);
      await Promise.all([useAppStore.getState().loadLogs(), useAppStore.getState().loadStats(), useAppStore.getState().getPendingFiles()]);
    } catch (error) { setError(errorText(error)); setConfirm(false); }
    finally { setBusy(false); }
  }
  async function classify() {
    if (!preview || !preview.entries.length) return;
    setClassifying(true); setAiError(''); setTrashConfirm(false);
    try {
      const files = await invoke<Verdict[]>('classify_files_cmd', {
        paths: preview.entries.filter(entry => !entry.error).map(entry => entry.source),
      });
      setVerdicts(Object.fromEntries(files.map(file => [file.path, file])));
    } catch (error) {
      const raw = String(error);
      if (raw.includes('ai.not_configured')) setAiError(t('beta.aiNotConfigured'));
      else if (raw.includes('ai.disabled')) setAiError(t('beta.aiDisabled'));
      else {
        const match = raw.match(/ai\.request_failed:\s*(.*)/s);
        setAiError(match ? t('beta.aiFailed', { error: match[1] }) : errorText(error));
      }
    } finally { setClassifying(false); }
  }
  async function trashPicks() {
    if (!preview) return;
    const picks = preview.entries.filter(entry => verdicts[entry.source]?.verdict === 'trash' && !entry.error);
    if (!picks.length) return;
    setBusy(true); setError(''); setTrashConfirm(false);
    try {
      const outcome = await invoke<OperationResult[]>('trash_files_cmd', { paths: picks.map(entry => entry.source) });
      setResults(outcome);
      const done = new Set(outcome.filter(result => result.success).map(result => result.source));
      if (done.size) {
        setPreview(current => current ? { ...current, entries: current.entries.filter(entry => !done.has(entry.source)) } : current);
        setSelected(current => { const next = new Set(current); picks.forEach(entry => { if (done.has(entry.source)) next.delete(entry.id); }); return next; });
        setVerdicts(current => { const next = { ...current }; done.forEach(source => { delete next[source]; }); return next; });
        await Promise.all([useAppStore.getState().loadLogs(), useAppStore.getState().loadStats(), useAppStore.getState().getPendingFiles()]);
      }
    } catch (error) { setError(errorText(error)); }
    finally { setBusy(false); }
  }
  const toggle = (id: string) => setSelected(old => { const next = new Set(old); if (next.has(id)) next.delete(id); else next.add(id); return next; });
  const entries = preview?.entries ?? [];
  const verdictList = entries.map(entry => verdicts[entry.source]).filter((verdict): verdict is Verdict => !!verdict);
  const keepCount = verdictList.filter(verdict => verdict.verdict === 'keep').length;
  const trashCount = verdictList.filter(verdict => verdict.verdict === 'trash').length;
  const unsureCount = verdictList.filter(verdict => verdict.verdict === 'unsure').length;
  const hasVerdicts = verdictList.length > 0;
  const labelFor = (verdict: string) => t(verdict === 'trash' ? 'beta.aiTrash' : verdict === 'keep' ? 'beta.aiKeep' : 'beta.aiUnsure');
  return <section className="space-y-4" aria-busy={busy || classifying}>
    <div className="flex flex-wrap justify-between items-center gap-3"><h2 className="text-lg font-semibold">{t('beta.review')}</h2><button className={buttonClass} onClick={refresh} disabled={busy || classifying}>{t('beta.refresh')}</button></div>
    <p className="text-sm text-text-muted">{t('beta.previewHelp')}</p>
    {busy && <p role="status">{t('beta.loading')}</p>}
    {error && <p role="alert" className="text-sm text-red-700 dark:text-red-300">{error}</p>}
    <Results results={results} />
    {preview && <>
      <div className="flex flex-wrap items-center gap-2">
        <button className={buttonClass} disabled={busy || classifying || confirm} onClick={() => setSelected(new Set(preview.entries.filter(e => !e.error && !e.warning && e.action !== 'delete').map(e => e.id)))}>{t('beta.selectAll')}</button>
        <button className={buttonClass} disabled={busy || classifying || confirm} onClick={() => setSelected(new Set())}>{t('beta.clearSelection')}</button>
        <button className={buttonClass} disabled={busy || classifying || !preview.entries.length} onClick={classify}>{classifying ? t('beta.aiClassifying') : t('beta.aiClassify')}</button>
        <span className="text-sm">{t('beta.selected', { count: selected.size })}</span>
      </div>
      <p className="text-xs text-text-muted">{t('beta.aiHelp')}</p>
      {aiError && <p role="alert" className="text-sm text-red-700 dark:text-red-300">{aiError}</p>}
      {hasVerdicts && <div className="space-y-2">
        <p className="text-sm">{t('beta.aiSummary', { keep: keepCount, trash: trashCount, unsure: unsureCount })}</p>
        <div className="flex flex-wrap items-center gap-2">
          <button className={buttonClass} disabled={busy || classifying || confirm || !keepCount} onClick={() => setSelected(new Set(preview.entries.filter(e => !e.error && verdicts[e.source]?.verdict === 'keep').map(e => e.id)))}>{t('beta.aiSelectKeep')}</button>
          <button className={buttonClass} disabled={busy || classifying || confirm || !trashCount} onClick={() => setTrashConfirm(true)}>{t('beta.aiTrashPicks', { count: trashCount })}</button>
        </div>
      </div>}
      {trashConfirm && <div className="rounded-lg border border-red-500/60 p-4 space-y-3" role="region" aria-label={t('beta.aiTrashPicks', { count: trashCount })}>
        <p className="text-sm">{t('beta.aiTrashConfirm', { count: trashCount })}</p>
        <div className="flex gap-2"><button className={primaryClass} disabled={busy} onClick={trashPicks}>{t('beta.aiTrashPicks', { count: trashCount })}</button><button className={buttonClass} disabled={busy} onClick={() => setTrashConfirm(false)}>{t('beta.cancel')}</button></div>
      </div>}
      {!preview.entries.length && <p className="py-8 text-text-muted">{t('beta.empty')}</p>}
      {confirm && <div className="rounded-lg border border-primary p-4 space-y-3" role="region" aria-label={t('beta.apply')}>
        <p className="text-sm">{t('beta.confirmHelp')}</p>
        <div className="flex gap-2"><button className={primaryClass} disabled={busy} onClick={apply}>{t('beta.confirm', { count: selected.size })}</button><button className={buttonClass} disabled={busy} onClick={() => setConfirm(false)}>{t('beta.cancel')}</button></div>
      </div>}
      <div className="space-y-2">{preview.entries.filter(e => !confirm || selected.has(e.id)).map(entry => <div key={entry.id} className="rounded-lg border border-border p-3">
        <label className="flex gap-3 items-start"><input type="checkbox" className="mt-1" checked={selected.has(entry.id)} disabled={busy || confirm || !!entry.error} onChange={() => toggle(entry.id)} />
          <span className="min-w-0 text-sm break-all"><span className="block font-medium">{entry.source}</span><span className="block mt-1">{entry.rule} · {t(`beta.${entry.action}`)} · {entry.size.toLocaleString()} B</span><span className="block text-text-muted">{entry.destination || t('settings.rules.recycleBin')}</span></span>
        </label>
        {verdicts[entry.source] && <div className="mt-2">
          <span className={`inline-block rounded px-2 py-0.5 text-xs font-medium ${verdictClass[verdicts[entry.source].verdict] || verdictClass.unsure}`}>{labelFor(verdicts[entry.source].verdict)} · {verdicts[entry.source].confidence}%</span>
          <span className="mt-1 block text-xs text-text-muted">{['keep', 'trash', 'unsure'].map(key => `${labelFor(key)} ${Math.round((verdicts[entry.source].probabilities[key] ?? 0) * 100)}%`).join(' · ')}</span>
        </div>}
        {entry.warning && <p className="mt-2 text-sm text-amber-800 dark:text-amber-200">{t('beta.warning')}</p>}
        {entry.error && <p className="mt-2 text-sm text-text-muted">{errorText(entry.error)}</p>}
      </div>)}</div>
      {!confirm && <button className={primaryClass} disabled={busy || classifying || !selected.size} onClick={() => setConfirm(true)}>{t('beta.apply')}</button>}
    </>}
  </section>;
}
