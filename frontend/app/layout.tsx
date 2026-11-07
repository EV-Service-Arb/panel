import type { Metadata, Viewport } from "next";
import type { ReactNode } from "react";

import { DocumentLang } from "@/shared/i18n";

import "./globals.css";

export const metadata: Metadata = {
  title: "Service-Arb panel",
  // Internal, behind sign-in: nothing here is for a search engine.
  robots: { index: false, follow: false },
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" data-theme={process.env.NEXT_PUBLIC_NAVY_THEME === "1" ? "oldschool" : undefined}>
      <body className="min-h-svh bg-background font-sans text-ink antialiased">
        <DocumentLang />
        {children}
      </body>
    </html>
  );
}
