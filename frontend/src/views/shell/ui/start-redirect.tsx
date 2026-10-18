"use client";

import { useRouter } from "next/navigation";
import { useEffect, useRef } from "react";

import { startRouteFor, useMe } from "@/entities/session";
import { takeReturnPage } from "@/shared/api";

/** `/`: where the caller starts (spec §10), or the page they were on when the sign-in dialog opened. */
export function StartRedirect() {
  const me = useMe();
  const router = useRouter();
  // Taken once: the read clears the key, and `me` changing re-runs the effect.
  const back = useRef<string | null | undefined>(undefined);
  useEffect(() => {
    back.current ??= takeReturnPage();
    router.replace(back.current !== null && back.current !== "/" ? back.current : startRouteFor(me));
  }, [me, router]);
  return null;
}
