import { useEffect, useRef, useState } from "react";

import { CHIP_ATTRIBUTE_NAMES, CHIP_TAG, type ChipAttributes } from "../model/attributes";

function applyAttributes(element: HTMLElement, attributes: ChipAttributes): void {
  for (const name of CHIP_ATTRIBUTE_NAMES) {
    const value = attributes[name];
    if (value === undefined) element.removeAttribute(name);
    else if (element.getAttribute(name) !== value) element.setAttribute(name, value);
  }
}

/**
 * Mounts the chip element into `hostRef` once its bundle is `ready`, keeps its attributes on
 * the latest values and forwards its `signout` event. Imperative rather than JSX: the element
 * must exist only after its definition, with its attributes set before `connectedCallback`.
 *
 * `painted` turns true when the element releases its reserve. The bundle holds the element at
 * 178×42 with an inline `visibility: hidden` until its stylesheet applies and lifts both in the
 * frame the chip paints; that inline style is the only readiness signal bundle v2 gives a host.
 *
 * `remountKey` (the panel's locale): the chip reads `<html lang>` only when it renders, and a
 * host attribute change is not a language change, so a new language needs a new element.
 */
export function useChipElement(ready: boolean, attributes: ChipAttributes, onSignOut: () => void, remountKey: string) {
  const hostRef = useRef<HTMLDivElement>(null);
  const elementRef = useRef<HTMLElement | null>(null);
  const latest = useRef({ attributes, onSignOut });
  const [painted, setPainted] = useState(false);

  useEffect(() => {
    latest.current = { attributes, onSignOut };
  });

  useEffect(() => {
    const host = hostRef.current;
    if (!ready || !host) return;
    const element = document.createElement(CHIP_TAG);
    applyAttributes(element, latest.current.attributes);
    const forwardSignOut = () => latest.current.onSignOut();
    element.addEventListener("signout", forwardSignOut);
    const observer = new MutationObserver(() => {
      if (element.style.visibility !== "hidden") setPainted(true);
    });
    observer.observe(element, { attributes: true, attributeFilter: ["style"] });
    host.appendChild(element);
    elementRef.current = element;
    return () => {
      observer.disconnect();
      element.removeEventListener("signout", forwardSignOut);
      element.remove();
      elementRef.current = null;
    };
  }, [ready, remountKey]);

  // Serialised so a fresh but equal object (every render) does not touch the element.
  const attributesKey = JSON.stringify(attributes);
  useEffect(() => {
    const element = elementRef.current;
    if (element) applyAttributes(element, latest.current.attributes);
  }, [attributesKey]);

  return { hostRef, painted };
}
