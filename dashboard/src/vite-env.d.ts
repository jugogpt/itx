/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** Base URL of the hub this dashboard talks to. Defaults to
   * `http://127.0.0.1:9100` (see `api.ts`) if unset. */
  readonly VITE_HUB_URL?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
