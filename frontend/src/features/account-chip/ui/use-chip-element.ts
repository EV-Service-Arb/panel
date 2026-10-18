import { useEffect, useRef, useState } from "react";

import { CHIP_ATTRIBUTE_NAMES, type ChipAttributes } from "../model/attributes";
import { routeOfNavigateEvent } from "../model/navigation";

function applyAttributes(element: HTMLElement, attributes: ChipAttributes): void {
  for (const name of CHIP_ATTRIBUTE_NAMES) {
    const value = attributes[name];
    if (value === undefined) element.removeAttribute(name);
    else if (element.getAttribute(name) !== value) element.setAttribute(name, value);
  }
}

/**
 * Mounts the chip element into `hostRef` once its bundle is `ready`, keeps its attributes on
 * the latest values and forwards its `signout` and `ev:navigate` events. Imperative rather than JSX: the element
 * must exist only after its definition, with its attributes set before `connectedCallback`.
 *
 * `painted` turns true when the element releases its reserve. The bundle holds the element at
 * 178×42 with an inline `visibility: hidden` until its stylesheet applies and lifts both in the
 * frame the chip paints; that inline style is the only readiness signal bundle v2 gives a host.
 *
 * `ev:navigate` (cancelable, `detail.href`): the chip renders plain `<a>`s outside the panel's
 * React tree, so it has no router; for a link to one of the panel's screens the host cancels
 * the event and navigates itself, anything else is left to the browser. A chip that does not
 * send the event (before banking's next release) simply navigates in full.
 *
 * `remountKey` (the panel's locale): the chip reads `<html lang>` only when it renders, and a
 * host attribute change is not a language change, so a new language needs a new element.
 */
export interface ChipHandlers {
  onSignOut: () => void;
  /** A panel route as `panelRouteFor` spells it. */
  onNavigate: (route: string) => void;
}

export function useChipElement(tag: string, ready: boolean, attributes: ChipAttributes, handlers: ChipHandlers, remountKey: string) {
  const hostRef = useRef<HTMLDivElement>(null);
  const elementRef = useRef<HTMLElement | null>(null);
  const latest = useRef({ attributes, handlers });
  const [painted, setPainted] = useState(false);

  useEffect(() => {
    latest.current = { attributes, handlers };
  });

  useEffect(() => {
    const host = hostRef.current;
    if (!ready || !host) return;
    const element = document.createElement(tag);
    applyAttributes(element, latest.current.attributes);
    const forwardSignOut = () => latest.current.handlers.onSignOut();
    const forwardNavigate = (event: Event) => {
      const route = routeOfNavigateEvent(event);
      if (route === null) return;
      event.preventDefault();
      latest.current.handlers.onNavigate(route);
    };
    element.addEventListener("signout", forwardSignOut);
    element.addEventListener("ev:navigate", forwardNavigate);
    const observer = new MutationObserver(() => {
      if (element.style.visibility !== "hidden") setPainted(true);
    });
    observer.observe(element, { attributes: true, attributeFilter: ["style"] });
    host.appendChild(element);
    elementRef.current = element;
    return () => {
      observer.disconnect();
      element.removeEventListener("signout", forwardSignOut);
      element.removeEventListener("ev:navigate", forwardNavigate);
      element.remove();
      elementRef.current = null;
    };
  }, [tag, ready, remountKey]);

  // Serialised so a fresh but equal object (every render) does not touch the element.
  const attributesKey = JSON.stringify(attributes);
  useEffect(() => {
    const element = elementRef.current;
    if (element) applyAttributes(element, latest.current.attributes);
  }, [attributesKey]);

  return { hostRef, painted };
}
