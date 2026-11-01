"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import { ApiError, type ApiFailure } from "@/shared/api";

import { type LiveTopic, useLiveSignal } from "./live";

export type Resource<T> =
  | { status: "loading" }
  | { status: "ok"; data: T }
  | { status: "error"; failure: ApiFailure | { kind: "invalid"; message: string } };

function failureOf(e: unknown) {
  if (e instanceof ApiError) return e.failure;
  return { kind: "invalid" as const, message: e instanceof Error ? e.message : String(e) };
}

export interface ResourceOptions {
  /** The live topic this read follows; a resync and a reconnect re-read it too. */
  live?: LiveTopic;
}

interface Answered<T> {
  key: string;
  group: string;
  tick: number;
  value: Resource<T>;
}

/**
 * What a read shows for `key` at `tick`, given the last answer that landed.
 * `fresh` is false while that answer is to an earlier key or tick of the same
 * group: shown so the screen does not blank, but not yet what was asked for.
 */
function shownOf<T>(answered: Answered<T>, key: string, group: string, tick: number): { value: Resource<T>; fresh: boolean } {
  const fresh = answered.key === key && answered.tick === tick;
  if (fresh) return { value: answered.value, fresh };
  const stale = answered.group === group && answered.value.status === "ok";
  return { value: stale ? answered.value : { status: "loading" }, fresh };
}

/**
 * One read, redone whenever `key` changes or `reload` is called. The answer to
 * an earlier key never overwrites a later one.
 *
 * Stale-while-revalidate: a reload, or a new key in the same `group` (the same
 * record at a newer version), keeps showing the last answer until the next one
 * lands, so a refresh after an action does not blank the screen. `fresh` tells
 * the two apart: anything that must act on the answer asked for — not the one
 * before it — waits for it.
 *
 * A live re-read (a change on a followed topic, a resync) is quiet: if it
 * fails, the last answer stays on screen — the person did not ask for it, and
 * a blip in the background must not replace figures with an error.
 */
export function useResource<T>(
  key: string,
  load: () => Promise<T>,
  group: string = key,
  options: ResourceOptions = {},
): Resource<T> & { reload: () => void; fresh: boolean } {
  const [state, setState] = useState<Answered<T>>({
    key,
    group,
    tick: 0,
    value: { status: "loading" },
  });
  const [tick, setTick] = useState(0);
  const quiet = useRef(false);
  const reload = useCallback(() => setTick((n) => n + 1), []);
  const refresh = useCallback(() => {
    quiet.current = true;
    setTick((n) => n + 1);
  }, []);
  useLiveSignal(options.live, refresh);

  useEffect(() => {
    let live = true;
    const background = quiet.current;
    quiet.current = false;
    load().then(
      (data) => live && setState({ key, group, tick, value: { status: "ok", data } }),
      (e: unknown) =>
        live &&
        setState((prev) =>
          background && prev.group === group && prev.value.status === "ok"
            ? { ...prev, key, tick }
            : { key, group, tick, value: { status: "error", failure: failureOf(e) } },
        ),
    );
    return () => {
      live = false;
    };
    // `load` is a fresh closure every render; `key` names what it reads.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, tick]);

  const { value, fresh } = shownOf(state, key, group, tick);
  return { ...value, reload, fresh };
}
