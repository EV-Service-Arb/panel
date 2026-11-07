"use client";

import { Button, Label, Popover, PopoverContent, PopoverTrigger, Switch } from "@evinvest/uikit";
import { Menu } from "lucide-react";
import { useEffect, useId, useState } from "react";
import { createPortal } from "react-dom";

import { useT } from "@/shared/i18n";
import { showPlaces, usePlacesShown } from "@/shared/map";

/** What the map draws besides the GMBs, behind a button stacked above Google's zoom. */
export function MapLayers({ map }: { map: google.maps.Map }) {
  const t = useT();
  const shown = usePlacesShown();
  const id = useId();
  const [box] = useState(() => {
    const div = document.createElement("div");
    div.style.margin = "10px";
    return div;
  });
  useEffect(() => {
    const controls = map.controls[google.maps.ControlPosition.RIGHT_BOTTOM];
    if (controls === undefined) throw new Error("the map has no bottom-right control slot");
    controls.push(box);
    return () => {
      const at = controls.getArray().indexOf(box);
      if (at < 0) throw new Error("the layers' control left the map by itself");
      controls.removeAt(at);
    };
  }, [map, box]);
  return createPortal(
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="outline" size="sm" icon className="bg-background shadow-md" aria-label={t("home.layers")} title={t("home.layers")}>
          <Menu aria-hidden />
        </Button>
      </PopoverTrigger>
      <PopoverContent side="left" align="end" className="w-64">
        <div className="flex items-start gap-3">
          <div className="flex min-w-0 flex-1 flex-col gap-0.5">
            <Label htmlFor={id}>{t("home.layers.places")}</Label>
            <p className="text-xs text-ink-soft">{t("home.layers.places.hint")}</p>
          </div>
          <Switch id={id} checked={shown} onCheckedChange={showPlaces} />
        </div>
      </PopoverContent>
    </Popover>,
    box,
  );
}
