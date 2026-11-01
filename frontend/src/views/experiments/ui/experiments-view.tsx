"use client";

import { Settled, Skeleton } from "@evinvest/uikit";

import { fetchExperiments } from "@/entities/experiment";
import { brandsOf, usePlaces } from "@/entities/place";
import { may, useMe } from "@/entities/session";
import { useFilterParams } from "@/features/funnel-filters";
import { ROUTES } from "@/shared/config/routes";
import { useT } from "@/shared/i18n";
import { useResource } from "@/shared/lib/use-resource";
import { EmptyState } from "@/shared/ui/empty-state";
import { ErrorState } from "@/shared/ui/error-state";
import { FilterSelect } from "@/shared/ui/filter-select";
import { ScreenFrame } from "@/shared/ui/screen-frame";

import { byBrand } from "../model/group";
import { BrandExperiments } from "./brand-experiments";

/**
 * The landings' A/B tests as config: what each brand declared, the split it
 * runs, and for an admin the kill switch and the weights. The numbers are
 * PostHog's, a link away; the panel does not count them again.
 */
export function ExperimentsView() {
  const t = useT();
  const me = useMe();
  const { brand, update } = useFilterParams();
  const places = usePlaces();
  const key = `experiments:${brand ?? ""}`;
  const data = useResource(key, () => fetchExperiments(brand), key, { live: ["experiments"] });
  const list = data.status === "ok" ? data.data : [];
  const brands = brandsOf([...places, ...list], brand);

  return (
    <ScreenFrame
      title={t("experiments.title")}
      back={ROUTES.more}
      actions={<FilterSelect label={t("filter.brand")} allLabel={t("filter.brand.all")} value={brand} options={brands.map((b) => ({ value: b, label: b }))} onChange={(b) => update({ brand: b })} />}
    >
      <Settled loading={data.status === "loading"} skeleton={<Skeleton className="h-64 w-full" />} className="flex flex-col gap-4">
        {data.status === "error" && <ErrorState failure={data.failure} onRetry={data.reload} />}
        {data.status === "ok" && list.length === 0 && <EmptyState title={t("experiments.empty")} description={t("experiments.empty.body")} />}
        {data.status === "ok" && list.length > 0 && (
          <>
            <p className="px-1 text-sm text-ink-soft">{t("experiments.how")}</p>
            {byBrand(list).map(([b, experiments]) => (
              <BrandExperiments key={b} brand={b} experiments={experiments} editable={may(me, "sa:sites:analysis:experiments:edit")} onSaved={data.reload} />
            ))}
          </>
        )}
      </Settled>
    </ScreenFrame>
  );
}
