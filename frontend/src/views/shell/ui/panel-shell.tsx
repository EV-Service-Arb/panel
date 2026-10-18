"use client";

import { Toaster } from "@evinvest/uikit";
import { type ReactNode, useState } from "react";

import { MeProvider, fetchMe } from "@/entities/session";
import { goToSignIn } from "@/shared/api";
import { LiveProvider } from "@/shared/lib/live";
import { DESKTOP_QUERY, useMediaQuery } from "@/shared/lib/use-media-query";
import { useResource } from "@/shared/lib/use-resource";
import { ErrorState } from "@/shared/ui/error-state";

import { NoAccessScreen, ShellSkeleton, UnavailableScreen } from "./access-screens";
import { ShellFrame } from "./shell-frame";
import { SignInOverlay } from "./signin-overlay";

/**
 * Everything behind sign-in. `/api/v1/me` decides what shows: a 401 has already
 * sent the browser to `/auth/login`, or opened the sign-in overlay when that is on (the skeleton stays up meanwhile), a 403 is
 * "no access", a 503 is "try again" without signing anyone out. The live socket
 * can end the session the same two ways later (4401, 4403).
 */
export function PanelShell({ children }: { children: ReactNode }) {
  // Mounted from the first render: the 401 of `/me` can land before anything else has.
  return (
    <>
      <PanelBody>{children}</PanelBody>
      <SignInOverlay />
    </>
  );
}

function PanelBody({ children }: { children: ReactNode }) {
  const me = useResource("me", fetchMe);
  const isDesktop = useMediaQuery(DESKTOP_QUERY);
  const [accessLost, setAccessLost] = useState(false);

  if (accessLost) return <NoAccessScreen />;
  if (me.status === "loading") return <ShellSkeleton />;
  if (me.status === "error") {
    switch (me.failure.kind) {
      case "unauthenticated":
        return <ShellSkeleton />;
      case "no_access":
        return <NoAccessScreen />;
      case "unavailable":
        return <UnavailableScreen onRetry={me.reload} />;
      default:
        return (
          <div className="p-6">
            <ErrorState failure={me.failure} onRetry={me.reload} />
          </div>
        );
    }
  }

  return (
    <MeProvider me={me.data}>
      <LiveProvider onUnauthenticated={goToSignIn} onForbidden={() => setAccessLost(true)}>
        <ShellFrame>{children}</ShellFrame>
      </LiveProvider>
      {/* On a phone the bottom is the tab bar and the sheets; toasts come from the top. */}
      <Toaster position={isDesktop ? "bottom-right" : "top-center"} />
    </MeProvider>
  );
}
