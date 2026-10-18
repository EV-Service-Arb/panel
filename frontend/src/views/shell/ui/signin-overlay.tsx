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
  redirectToSignIn,
  registerOverlayHost,
  subscribeOverlay,
} from "@/shared/api";
import { useT } from "@/shared/i18n";
import { useButtonSize } from "@/shared/ui/touch";

/** Past this the person is sent to the full-page sign-in instead of waiting on a bundle that may never come. */
const BUNDLE_BUDGET_MS = 5000;

/**
 * Same-origin on purpose: the panel's CSP is `script-src 'self'; connect-src 'self'`, so neither
 * evinvest.ltd's registry nor its bundle can be fetched from here. The panel server proxies the
 * bundle at this path.
 */
const SIGN_IN_SCRIPT = "/auth/mfe/sign-in.js";

/** True once the sign-in element is defined and new enough; false: script unreachable, blocked, too old or slow. */
async function loadSignIn(): Promise<boolean> {
  try {
    return await loadElement(SIGN_IN_TAG, SIGN_IN_SCRIPT, { minApiVersion: API_VERSION, timeoutMs: BUNDLE_BUDGET_MS });
  } catch {
    // The same answer as a timeout: the old redirect.
    return false;
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
  const [ready, setReady] = useState(false);

  useEffect(() => registerOverlayHost(), []);

  useEffect(() => {
    if (state !== "open" || ready) return;
    let stale = false;
    void loadSignIn().then(ok => {
      if (stale) return;
      if (ok) setReady(true);
      else redirectToSignIn();
    });
    return () => {
      stale = true;
    };
  }, [state, ready]);

  if (state === "idle") return null;
  return (
    <>
      {ready && (
        <SignInDialog
          scriptUrl={SIGN_IN_SCRIPT}
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
