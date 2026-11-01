"use client";

import { Button, Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle, Skeleton } from "@evinvest/uikit";

import { RequestAccess } from "@/features/request-access";
import { useT } from "@/shared/i18n";
import { useButtonSize } from "@/shared/ui/touch";

/** Signed in to EV, but without the panel's work: asks the admins for `sa:operator`. */
export function NoAccessScreen() {
  return <RequestAccess need="sa:operator" continueTo={null} />;
}

/** 503: concierge unreachable. The session is kept, so this only offers to try again. */
export function UnavailableScreen({ onRetry }: { onRetry: () => void }) {
  const t = useT();
  const button = useButtonSize();
  return (
    <Empty className="min-h-svh">
      <EmptyHeader>
        <EmptyTitle>{t("state.unavailable.title")}</EmptyTitle>
        <EmptyDescription>{t("state.unavailable.body")}</EmptyDescription>
      </EmptyHeader>
      <EmptyContent>
        <Button size={button()} onClick={onRetry}>{t("state.retry")}</Button>
      </EmptyContent>
    </Empty>
  );
}

export function ShellSkeleton() {
  const t = useT();
  return (
    <div className="flex min-h-svh flex-col" aria-busy="true" aria-label={t("state.loading")}>
      <Skeleton className="h-14 w-full rounded-none" />
      <div className="flex flex-1 flex-col gap-3 p-6">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-32 w-full" />
        <Skeleton className="h-32 w-full" />
      </div>
    </div>
  );
}
