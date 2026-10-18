// Small wrappers around Radix primitives, styled in app.css.

import { Check, ChevronDown, X } from 'lucide-react';
import { AlertDialog, Select as RSelect, Toast } from 'radix-ui';
import { createContext, ReactNode, useCallback, useContext, useEffect, useRef, useState } from 'react';
import { onHubChange } from './api';

// --- Toasts -------------------------------------------------------------------

interface ToastItem {
  id: number;
  title: string;
  description?: string;
  tone: 'ok' | 'error';
}

const ToastContext = createContext<(toast: Omit<ToastItem, 'id'>) => void>(() => {});

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const next = useRef(0);
  const push = useCallback((toast: Omit<ToastItem, 'id'>) => {
    setToasts((all) => [...all.slice(-3), { ...toast, id: next.current++ }]);
  }, []);
  return (
    <ToastContext.Provider value={push}>
      <Toast.Provider swipeDirection="right" duration={5000}>
        {children}
        {toasts.map((t) => (
          <Toast.Root
            key={t.id}
            className={`toast ${t.tone}`}
            duration={t.tone === 'error' ? 10000 : 5000}
            onOpenChange={(open) => !open && setToasts((all) => all.filter((x) => x.id !== t.id))}
          >
            <Toast.Title className="toast-title">{t.title}</Toast.Title>
            {t.description && <Toast.Description className="toast-description">{t.description}</Toast.Description>}
            <Toast.Close className="icon-button toast-close" aria-label="Close">
              <X size={14} />
            </Toast.Close>
          </Toast.Root>
        ))}
        <Toast.Viewport className="toast-viewport" />
      </Toast.Provider>
    </ToastContext.Provider>
  );
}

export const useToast = () => useContext(ToastContext);

/** Runs an action, toasting its failure (and optionally its success). */
export function useAction() {
  const toast = useToast();
  return useCallback(
    async (action: () => Promise<unknown>, success?: string | (() => string)) => {
      try {
        await action();
        if (success) toast({ title: typeof success === 'string' ? success : success(), tone: 'ok' });
        return true;
      } catch (error) {
        toast({ title: 'Something went wrong', description: error instanceof Error ? error.message : String(error), tone: 'error' });
        return false;
      }
    },
    [toast],
  );
}

// --- Select -------------------------------------------------------------------

export interface Option {
  value: string;
  label: string;
  hint?: string;
  disabled?: boolean;
}

export function Select({
  value,
  onChange,
  options,
  label,
  disabled,
  className,
  placeholder,
}: {
  value: string;
  onChange: (value: string) => void;
  options: Option[];
  label: string;
  disabled?: boolean;
  className?: string;
  placeholder?: string;
}) {
  const current = options.find((o) => o.value === value);
  return (
    <RSelect.Root value={value} onValueChange={onChange} disabled={disabled}>
      <RSelect.Trigger className={`select-trigger ${className ?? ''}`} aria-label={label} data-select={label}>
        <span className="select-value">{current?.label ?? placeholder ?? label}</span>
        <RSelect.Icon className="select-icon">
          <ChevronDown size={14} />
        </RSelect.Icon>
      </RSelect.Trigger>
      <RSelect.Portal>
        <RSelect.Content className="select-content" position="popper" sideOffset={4} collisionPadding={8}>
          <RSelect.Viewport className="select-viewport">
            {options.map((o) => (
              <RSelect.Item key={o.value} value={o.value} disabled={o.disabled} className="select-item">
                <RSelect.ItemText>{o.label}</RSelect.ItemText>
                {o.hint && <span className="select-hint">{o.hint}</span>}
                <RSelect.ItemIndicator className="select-check">
                  <Check size={14} />
                </RSelect.ItemIndicator>
              </RSelect.Item>
            ))}
          </RSelect.Viewport>
        </RSelect.Content>
      </RSelect.Portal>
    </RSelect.Root>
  );
}

// --- Confirmation -------------------------------------------------------------

export function Confirm({
  trigger,
  title,
  description,
  action,
  onConfirm,
}: {
  trigger: ReactNode;
  title: string;
  description: string;
  action: string;
  onConfirm: () => void;
}) {
  return (
    <AlertDialog.Root>
      <AlertDialog.Trigger asChild>{trigger}</AlertDialog.Trigger>
      <AlertDialog.Portal>
        <AlertDialog.Overlay className="overlay" />
        <AlertDialog.Content className="dialog small">
          <AlertDialog.Title className="dialog-title">{title}</AlertDialog.Title>
          <AlertDialog.Description className="dialog-description">{description}</AlertDialog.Description>
          <div className="dialog-actions">
            <AlertDialog.Cancel className="button">Cancel</AlertDialog.Cancel>
            <AlertDialog.Action className="button danger-solid" onClick={onConfirm}>
              {action}
            </AlertDialog.Action>
          </div>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  );
}

// --- Misc -----------------------------------------------------------------------

export function StatusPill({ status, prefix = '' }: { status: string; prefix?: string }) {
  return <span className={`pill ${prefix}${status}`}>{status.replace('_', ' ')}</span>;
}

/** Calls `fn` now and then every `ms` while the page is visible. */
export function usePoll(fn: () => void, ms: number, deps: unknown[]) {
  const saved = useRef(fn);
  saved.current = fn;
  useEffect(() => {
    saved.current();
    const timer = setInterval(() => {
      if (document.visibilityState === 'visible') saved.current();
    }, ms);
    return () => clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  // Live updates: reload on hub changes (coalescing bursts, e.g. a streaming
  // agent) and as soon as the page is visible or online again.
  useEffect(() => {
    let pending: ReturnType<typeof setTimeout> | undefined;
    const visible = () => document.visibilityState === 'visible';
    const onChange = () => {
      if (!visible() || pending !== undefined) return;
      pending = setTimeout(() => {
        pending = undefined;
        saved.current();
      }, 250);
    };
    const onResume = () => {
      if (visible()) saved.current();
    };
    const unsubscribe = onHubChange(onChange);
    document.addEventListener('visibilitychange', onResume);
    window.addEventListener('online', onResume);
    window.addEventListener('pageshow', onResume);
    return () => {
      unsubscribe();
      clearTimeout(pending);
      document.removeEventListener('visibilitychange', onResume);
      window.removeEventListener('online', onResume);
      window.removeEventListener('pageshow', onResume);
    };
  }, []);
}

/** Grows a textarea with its content. */
export function AutoTextarea(props: React.TextareaHTMLAttributes<HTMLTextAreaElement>) {
  const ref = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, 320)}px`;
  }, [props.value]);
  return <textarea ref={ref} rows={2} {...props} />;
}

export const isMac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform);
export const submitShortcut = isMac ? '⌘↵' : 'Ctrl+↵';
