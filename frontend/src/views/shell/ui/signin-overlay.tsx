"use client";

import { SIGN_IN_TAG } from "@evinvest/concierge";
import { SignInDialog } from "@evinvest/concierge/react";
import { API_VERSION, loadElement } from "@evinvest/mfe";
import { Button, Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle } from "@evinvest/uikit";
import { useEffect, useState, useSyncExternalStore } from "react";

import {
  dismissOverlay,
  openOverlay,
  overlayState,
  platformMfe,
  redirectToSignIn,
  registerOverlayHost,
  subscribeOverlay,
} from "@/shared/api";
import { useT } from "@/shared/i18n";
import { useButtonSize } from "@/shared/ui/touch";

/** Past this the person is sent to the full-page sign-in instead of waiting on a bundle that may never come. */
const BUNDLE_BUDGET_MS = 5000;

/**
 * Same-origin unless the platform source is on (`NEXT_PUBLIC_MFE_PLATFORM=1`, which needs the
 * server's PANEL_CSP_PLATFORM_MFE=1 too): the CSP is then `'self'` only, so the panel server
 * proxies the bundle at this path.
 */
const SIGN_IN_SCRIPT = "/auth/mfe/sign-in.js";

/** The bundle URL once the sign-in element is defined and new enough; null: script unreachable, blocked, too old or slow. */
async function loadSignIn(): Promise<string | null> {
  try {
    const scriptUrl = await platformMfe.scriptUrl("signIn", SIGN_IN_SCRIPT, BUNDLE_BUDGET_MS);
    if (scriptUrl === null) return null;
    const ok = await loadElement(SIGN_IN_TAG, scriptUrl, { minApiVersion: API_VERSION, timeoutMs: BUNDLE_BUDGET_MS });
    return ok ? scriptUrl : null;
  } catch {
    // The same answer as a timeout: the old redirect.
    return null;
  }
}

/**
 * The concierge sign-in dialog over the dimmed panel, shown when a 401 arrives and the overlay is
 * on. The element draws its own backdrop. When it signs in, the existing RP login runs
 * (`/auth/login` -> authorize -> callback); when the bundle cannot be had in time, so does the
 * redirect it replaces.
 */
export function SignInOverlay() {
  const t = useT();
  const button = useButtonSize();
  const state = useSyncExternalStore(subscribeOverlay, overlayState, () => "idle");
  const [scriptUrl, setScriptUrl] = useState<string | null>(null);

  useEffect(() => registerOverlayHost(), []);

  useEffect(() => {
    if (state !== "open" || scriptUrl !== null) return;
    let stale = false;
    void loadSignIn().then(url => {
      if (stale) return;
      if (url !== null) setScriptUrl(url);
      else redirectToSignIn();
    });
    return () => {
      stale = true;
    };
  }, [state, scriptUrl]);

  if (state === "idle") return null;
  return (
    <>
      {scriptUrl !== null && (
        <SignInDialog
          scriptUrl={scriptUrl}
          open={state === "open"}
          onOpenChange={open => {
            if (!open) dismissOverlay();
          }}
          onSignedIn={redirectToSignIn}
        />
      )}
      {state === "dismissed" && (
        <Empty className="fixed inset-0 z-50 bg-background">
          <EmptyHeader>
            <EmptyTitle>{t("signin.expired.title")}</EmptyTitle>
            <EmptyDescription>{t("signin.expired.body")}</EmptyDescription>
          </EmptyHeader>
          <EmptyContent>
            <Button size={button()} onClick={openOverlay}>{t("signin.expired.action")}</Button>
          </EmptyContent>
        </Empty>
      )}
    </>
  );
}
