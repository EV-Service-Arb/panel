import { Suspense } from "react";

import { AccountView } from "@/views/account";

export default function Page() {
  // The view reads `?section=`, which a static export only has in the browser.
  return (
    <Suspense>
      <AccountView />
    </Suspense>
  );
}
