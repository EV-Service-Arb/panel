"use client";

import { useRouter } from "next/navigation";
import { useEffect, useRef } from "react";

import { takeReturnPage } from "@/shared/api";
import { useT } from "@/shared/i18n";

/** `/`: the caller's GMBs, unless they come back from the sign-in dialog to the page they were on. */
export function Home() {
  const t = useT();
  const router = useRouter();
  // Taken once: the read clears the key.
  const back = useRef<string | null | undefined>(undefined);
  useEffect(() => {
    back.current ??= takeReturnPage();
    if (back.current !== null && back.current !== "/") router.replace(back.current);
  }, [router]);
  return <p className="p-6 text-ink-soft">{t("home.map")}</p>;
}
