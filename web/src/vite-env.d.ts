/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** Issue page for the changelog's ticket ids, `{id}` standing for e.g. `H15-12`; unset = no links. */
  readonly VITE_ISSUE_URL?: string;
}
