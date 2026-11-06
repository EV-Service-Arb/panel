"use client";

import { Button } from "@evinvest/uikit";
import { MapPinPlus, Move } from "lucide-react";
import { usePathname, useRouter } from "next/navigation";

import { type Gmb, setOnMap } from "@/entities/gmb";
import { useT } from "@/shared/i18n";
import { useButtonSize } from "@/shared/ui/touch";

/** Opens the home map to place a GMB Google has no point for, or to move the one placed: nothing for a point of Google's. */
export function PlaceButton({ gmb, onNavigate }: { gmb: Gmb; onNavigate?: () => void }) {
  const t = useT();
  const button = useButtonSize();
  const router = useRouter();
  const pathname = usePathname();
  if (gmb.location !== null && !gmb.location.placed) return null;
  const label = t(gmb.location === null ? "gmbs.place.set" : "gmbs.place.move");
  const open = () => {
    if (pathname !== "/") router.push("/");
    setOnMap({ placing: gmb.id, peeked: null });
    onNavigate?.();
  };
  return (
    <Button variant="ghost" size={button("sm")} icon aria-label={label} title={label} onClick={open}>
      {gmb.location === null ? <MapPinPlus aria-hidden /> : <Move aria-hidden />}
    </Button>
  );
}
