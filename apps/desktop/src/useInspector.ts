import { useCallback, useEffect, useRef, useState } from "react";
import { describeError, type BridgeClient, type ConnectionInfo } from "./bridge";
import type {
  BridgeError, OperationState, OperationView, OperationsPage, OutputFrame,
} from "./generated/contracts";

const PAGE_SIZE = 32;
const MAX_DISPLAY_FRAMES = 1200;
const MAX_DISPLAY_CHARACTERS = 512 * 1024;
type DisplayFrame = Pick<OutputFrame, "sequence" | "stream" | "text">;

export function isActive(state: OperationState): boolean {
  return state === "accepted" || state === "running";
}

function newer(previous: OperationView | null, next: OperationView): OperationView {
  if (!previous || previous.operation_id !== next.operation_id) return next;
  if (previous.updated_at_ms > next.updated_at_ms) return previous;
  if (!isActive(previous.state) && isActive(next.state)) return previous;
  return next;
}

export function useInspector(client: BridgeClient) {
  const [connection, setConnection] = useState<ConnectionInfo | null>(null);
  const [page, setPage] = useState<OperationsPage>({ operations: [], next_offset: null });
  const [offset, setOffset] = useState(0);
  const [listError, setListError] = useState<BridgeError | null>(null);
  const [listLoading, setListLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [lastSynced, setLastSynced] = useState<number | null>(null);
  const [refreshKey, setRefreshKey] = useState(0);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [operation, setOperation] = useState<OperationView | null>(null);
  const [frames, setFrames] = useState<DisplayFrame[]>([]);
  const [detailError, setDetailError] = useState<BridgeError | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [gap, setGap] = useState(false);
  const [trimmed, setTrimmed] = useState(false);
  const [hasMoreOutput, setHasMoreOutput] = useState(false);
  const [pendingCancellations, setPendingCancellations] = useState<Set<string>>(new Set());
  const pendingCancellationIds = useRef(new Set<string>());
  const [actionError, setActionError] = useState<BridgeError | null>(null);
  const [now, setNow] = useState(Date.now());
  const selection = useRef(selectedId);
  selection.current = selectedId;

  useEffect(() => {
    const clock = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(clock);
  }, []);

  useEffect(() => {
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      if (disposed) return;
      setRefreshing(true);
      try {
        const info = await client.connectionInfo();
        if (disposed) return;
        setConnection(info);
        const next = await client.listOperations({ offset, limit: PAGE_SIZE });
        if (disposed) return;
        setPage(next);
        setListError(null);
        setLastSynced(Date.now());
        setSelectedId((current) => current ?? next.operations[0]?.operation_id ?? null);
      } catch (error) {
        if (!disposed) setListError(describeError(error));
      } finally {
        if (!disposed) {
          setRefreshing(false);
          setListLoading(false);
          timer = setTimeout(poll, 3000);
        }
      }
    }
    void poll();
    return () => { disposed = true; clearTimeout(timer); };
  }, [client, offset, refreshKey]);

  useEffect(() => {
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    let cursor = 0;
    let first = true;
    let retained: DisplayFrame[] = [];
    setOperation(null);
    setFrames([]);
    setDetailError(null);
    setActionError(null);
    setGap(false);
    setTrimmed(false);
    setHasMoreOutput(false);
    setDetailLoading(Boolean(selectedId));
    if (!selectedId) return;
    const operationId = selectedId;

    async function poll() {
      let delay = 1000;
      try {
        if (first) {
          const value = await client.getOperation({ operation_id: operationId });
          if (disposed) return;
          if (value.operation_id !== operationId) throw { code: "response_mismatch", message: "The broker returned a different command." };
          setOperation((current) => newer(current, value));
          first = false;
        }
        // Drain a bounded number of pages per pass. Each reader owns its cursor.
        for (let batch = 0; batch < 4; batch++) {
          const value = await client.readOutput({ operation_id: operationId, cursor, max_bytes: 16384 });
          if (disposed) return;
          if (value.operation_id !== operationId || value.operation.operation_id !== operationId) {
            throw { code: "response_mismatch", message: "Output did not match the selected command." };
          }
          const fresh = value.frames
            .filter((frame) => frame.sequence > cursor)
            .map(({ sequence, stream, text }) => ({ sequence, stream, text }));
          retained = [...retained, ...fresh];
          let characters = retained.reduce((sum, frame) => sum + frame.text.length, 0);
          let removed = 0;
          while (retained.length - removed > MAX_DISPLAY_FRAMES || characters > MAX_DISPLAY_CHARACTERS) {
            characters -= retained[removed].text.length;
            removed++;
          }
          if (removed) {
            retained = retained.slice(removed);
            setTrimmed(true);
          }
          if (value.gap) setGap(true);
          setFrames(retained);
          cursor = value.next_cursor;
          setOperation((current) => newer(current, value.operation));
          setHasMoreOutput(value.has_more);
          setDetailError(null);
          setDetailLoading(false);
          if (!value.has_more) {
            // Completed output is stable. Manual refresh can read it again.
            if (!isActive(value.operation.state) && value.operation.output_complete) return;
            if (!isActive(value.operation.state)) delay = 3000;
            break;
          }
          delay = 100;
        }
      } catch (error) {
        if (disposed) return;
        setDetailError(describeError(error));
        setDetailLoading(false);
        delay = 3000;
      }
      if (!disposed) timer = setTimeout(poll, delay);
    }
    void poll();
    return () => { disposed = true; clearTimeout(timer); };
  }, [client, selectedId, refreshKey]);

  const refresh = useCallback(() => setRefreshKey((value) => value + 1), []);
  const changePage = useCallback((value: number) => {
    setPage({ operations: [], next_offset: null });
    setSelectedId(null);
    setListLoading(true);
    setOffset(value);
  }, []);

  const cancel = useCallback(async () => {
    if (!operation || selection.current !== operation.operation_id || !isActive(operation.state) || pendingCancellationIds.current.has(operation.operation_id)) return;
    const operationId = operation.operation_id;
    pendingCancellationIds.current.add(operationId);
    setPendingCancellations(new Set(pendingCancellationIds.current));
    setActionError(null);
    try {
      const value = await client.cancelOperation({ operation_id: operationId });
      if (value.operation_id !== operationId) throw { code: "response_mismatch", message: "The cancellation response did not match the selected command." };
      if (selection.current === operationId) setOperation((current) => newer(current, value));
    } catch (error) {
      if (selection.current === operationId) setActionError(describeError(error));
    } finally {
      pendingCancellationIds.current.delete(operationId);
      setPendingCancellations(new Set(pendingCancellationIds.current));
    }
  }, [client, operation]);

  return {
    connection, page, offset, listError, listLoading, refreshing, lastSynced,
    selectedId, select: setSelectedId,
    operation: operation?.operation_id === selectedId ? operation : null,
    frames, detailError, detailLoading,
    gap, trimmed, hasMoreOutput,
    cancelling: selectedId !== null && pendingCancellations.has(selectedId),
    actionError, now, refresh, changePage,
    cancel, connected: lastSynced !== null && !listError,
  };
}

export type Inspector = ReturnType<typeof useInspector>;
