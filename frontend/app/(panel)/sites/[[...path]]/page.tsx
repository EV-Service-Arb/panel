import { SitesView } from "@/views/sites";

// One page for every `/sites/<view>`: the export writes `sites/index.html`, which the backend serves for them all.
export function generateStaticParams() {
  return [{ path: [] }];
}

export default function Page() {
  return <SitesView />;
}
