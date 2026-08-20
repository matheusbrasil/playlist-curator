import { useCallback, useEffect, useRef, useState } from "react";
import { type CoreError, toCoreError } from "./ipc";

export type AsyncState<T> =
  | { status: "loading" }
  | { status: "error"; error: CoreError }
  | { status: "success"; data: T };

export type Async<T> = {
  state: AsyncState<T>;
  reload: () => void;
  /** Replace the loaded value locally, e.g. after a mutation returned the new one. */
  set: (data: T) => void;
};

/**
 * Loads once per change of `deps` and exposes the three states every call has.
 * Results arriving after unmount or after a newer run are discarded.
 */
export function useAsync<T>(load: () => Promise<T>, deps: readonly unknown[]): Async<T> {
  const [state, setState] = useState<AsyncState<T>>({ status: "loading" });
  const [nonce, setNonce] = useState(0);
  const runId = useRef(0);
  const loadRef = useRef(load);
  loadRef.current = load;

  useEffect(() => {
    const id = ++runId.current;
    let cancelled = false;
    setState({ status: "loading" });
    loadRef
      .current()
      .then((data) => {
        if (!cancelled && id === runId.current) setState({ status: "success", data });
      })
      .catch((err: unknown) => {
        if (!cancelled && id === runId.current) {
          setState({ status: "error", error: toCoreError(err) });
        }
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, nonce]);

  const reload = useCallback(() => setNonce((n) => n + 1), []);
  const set = useCallback((data: T) => {
    // Bump the run id so an in-flight load cannot overwrite this value.
    runId.current++;
    setState({ status: "success", data });
  }, []);

  return { state, reload, set };
}

export type ActionState = {
  running: boolean;
  error: CoreError | null;
  message: string | null;
};

/** One-shot mutations: a button that runs something and reports what happened. */
export function useAction(): ActionState & {
  run: <T>(fn: () => Promise<T>, onDone?: (result: T) => string | void) => Promise<T | null>;
  clear: () => void;
} {
  const [state, setState] = useState<ActionState>({
    running: false,
    error: null,
    message: null,
  });

  const run = useCallback(
    async <T,>(fn: () => Promise<T>, onDone?: (result: T) => string | void) => {
      setState({ running: true, error: null, message: null });
      try {
        const result = await fn();
        const message = onDone ? onDone(result) : undefined;
        setState({ running: false, error: null, message: message ?? null });
        return result;
      } catch (err) {
        setState({ running: false, error: toCoreError(err), message: null });
        return null;
      }
    },
    [],
  );

  const clear = useCallback(
    () => setState({ running: false, error: null, message: null }),
    [],
  );

  return { ...state, run, clear };
}
