import { type Infer, arrayOf, nullable, object, oneOf, str } from "@/shared/lib/parse";

import { SOURCE_KINDS } from "./generated";

export { KEYED_SOURCE_KINDS } from "./generated";
export type { KeyedSourceKind, SourceKind } from "./generated";

const sourceParser = object({
  key_id: str,
  kind: oneOf(SOURCE_KINDS),
  brands: arrayOf(str),
  created_at: str,
  revoked_at: nullable(str),
});
export type Source = Infer<typeof sourceParser>;

export const sourcesParser = object({ sources: arrayOf(sourceParser) });
export const addedSourceParser = object({ key_id: str, secret: str });
export type AddedSource = Infer<typeof addedSourceParser>;
