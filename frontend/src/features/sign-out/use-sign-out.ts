"use client";

import { platformSignOutHref } from "@evinvest/concierge";
import { useCallback } from "react";

import { signOut } from "@/entities/session";
import { useT } from "@/shared/i18n";
import { notifyFailure } from "@/shared/ui/notify";

/**
 * Sign out of the panel and then of evinvest.ltd. The panel's session alone is not enough: the
 * platform session would sign the same person back in silently at the next "Sign in". Every
 * way out (the chip's `signout`, its fallback, the More screen) comes here, so this navigation
 * is the one that wins; it goes where the chip's own navigation goes. Concierge's confirm page
 * comes back to `/`, which is in the `sa` scope's `return_paths`; with no session there the 401
 * of `/api/v1/me` opens the sign-in.
 */
export function useSignOut(): () => void {
  const t = useT();
  return useCallback(() => {
    signOut().then(
      () => window.location.assign(platformSignOutHref(`${window.location.origin}/`)),
      (e: unknown) => notifyFailure(e, t),
    );
  }, [t]);
}
